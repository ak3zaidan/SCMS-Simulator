//! Records the compiler, the target and the source revision in the binary, for the run
//! manifest.
//!
//! 02-architecture.md §6.5 and Phase 1 acceptance criterion 6 require the manifest to
//! carry the compiler version, the platform triple and a hash that pins the engine.
//! None of the three is knowable from inside a running program without spawning a
//! process, and the manifest must be reproducible, so they are captured here — at build
//! time, by the build itself — and embedded.
//!
//! This is not a wall-clock read: nothing here is a time, and the values are a property
//! of the build, which is exactly what the manifest is pinning. The one thing
//! deliberately *not* captured is a build timestamp; `Manifest::build_utc` is
//! caller-supplied and excluded from every digest for that reason.
//!
//! # The source revision
//!
//! The vertical-slice audit found `git_commit` recorded as `"unknown"` on every run, so
//! the manifest's "engine hash" pinned the compiler, the target and the crate versions
//! and said nothing at all about the source. It now asks git.
//!
//! Two facts are captured, not one:
//!
//! * `V2XW_GIT_COMMIT` — the commit, from `$V2XW_GIT_COMMIT` when the build environment
//!   sets one (CI does), else from `git rev-parse HEAD`, else `"unknown"`.
//! * `V2XW_GIT_DIRTY` — `"clean"`, `"dirty"` or `"unknown"`, from
//!   `git status --porcelain`.
//!
//! The second exists because the first on its own is a claim the build cannot support: a
//! commit hash beside a working tree with uncommitted edits names source that is *not*
//! what ran, and a manifest that stated the commit and stayed silent about the edits
//! would read as a guarantee it does not hold. `crate::manifest::assemble` turns a dirty
//! tree into a manifest warning for the same reason.
//!
//! A source tree with no `.git` — a vendored crate, a published tarball — answers
//! `"unknown"` to both, which is the honest answer there and is what the manifest says.
//!
//! # Rebuilds
//!
//! `cargo:rerun-if-changed` is emitted for `.git/HEAD` and for the file the current
//! branch's ref lives in, **only when those paths exist**: a `rerun-if-changed` naming a
//! path that is not there makes cargo rebuild the crate on every invocation. A commit
//! moves the ref file, so the constant is refreshed. A build that only *edits* the tree
//! does not move either, so the dirty flag can go stale until something else triggers the
//! script — which is why the flag is reported as a warning rather than folded into a
//! claim of exactness.

use std::path::Path;
use std::process::Command;

/// Runs a git command in the workspace root and returns its trimmed stdout on success.
///
/// `None` for a missing git, a non-zero exit (not a repository, for instance) or output
/// that is not UTF-8. Every failure is the same answer — "this build cannot tell you" —
/// and each is reported as such rather than guessed at.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        // The build script's working directory is the crate root; the workspace root is
        // two levels up, and `git` finds the repository from either. Naming it makes the
        // command independent of where cargo chose to run the script from.
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    Some(text.trim().to_string())
}

fn main() {
    // Re-run only when the toolchain pin, the source revision or this script changes.
    // Without these the script reruns on every source edit, which costs a `rustc -vV` and
    // two `git` invocations per build for no new information.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../rust-toolchain.toml");
    println!("cargo:rerun-if-env-changed=V2XW_GIT_COMMIT");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        // An unknown compiler is recorded as unknown rather than guessed: a manifest that
        // claims a version it did not measure is worse than one that admits it could not.
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=V2XW_RUSTC_VERSION={version}");

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=V2XW_TARGET={target}");

    // The git directory, when there is one. `git rev-parse --git-dir` answers for a
    // worktree and a submodule too, where `../../.git` is a file rather than a directory.
    let git_dir = git(&["rev-parse", "--absolute-git-dir"]);
    if let Some(dir) = &git_dir {
        let head = Path::new(dir).join("HEAD");
        if head.exists() {
            println!("cargo:rerun-if-changed={}", head.display());
        }
        // The file the current branch's tip lives in: `HEAD` itself does not change when
        // a commit is made on the branch it points at, the ref file does.
        if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
            let ref_path = Path::new(dir).join(&reference);
            if ref_path.exists() {
                println!("cargo:rerun-if-changed={}", ref_path.display());
            }
        }
    }

    // The environment wins: CI knows which revision it checked out, and a build from a
    // tarball has no repository to ask. A repository is the fallback, and "unknown" is
    // the answer when there is neither.
    let commit = std::env::var("V2XW_GIT_COMMIT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=V2XW_GIT_COMMIT={commit}");

    // Whether the tree that was compiled is the tree the commit names. `--porcelain`
    // prints one line per changed path and nothing at all for a clean tree, so an empty
    // answer is "clean" and any answer at all is "dirty".
    let dirty = match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(out) if out.is_empty() => "clean",
        Some(_) => "dirty",
        None => "unknown",
    };
    println!("cargo:rustc-env=V2XW_GIT_DIRTY={dirty}");

    // The scenario surface the page's settings form is generated from (§2 of
    // 13-product-direction.md). See the section below for why it is a build step.
    emit_reflection();
}

// ---------------------------------------------------------------------------
// Scenario schema reflection (13-product-direction.md §2)
// ---------------------------------------------------------------------------
//
// The settings interface is generated from the schema, so the schema has to be
// generated from the types. `serde` can tell a program what a *value* looks like and
// nothing about what a *type* accepts, and the one place a field's unit, its meaning and
// its optionality all live is the declaration in `src/scenario/schema.rs`. So the
// declarations are read here, at build time, and emitted as a table
// `crate::scenario::publish` walks.
//
// Why this is not a duplicate: nothing below names a scenario field. The table is
// whatever the source says, so a field added to a struct appears in the published schema
// on the next build and a field removed disappears. `src/scenario/publish.rs` carries a
// test that every leaf of the published schema is a key the loader round-trips, which is
// what catches a parse this script got wrong.
//
// # Why a text parse rather than a proc macro
//
// A derive macro would be the textbook answer and it cannot see doc comments' *intent*
// any better than this does; it would also put a `syn` dependency and a fourth crate in
// the build for one table. The parse below leans on two facts that hold for every file
// it reads: the tree is `rustfmt`-formatted, so a top-level item starts at column zero
// and a field of one is indented by exactly four; and `#![deny(missing_docs)]` in
// `src/lib.rs` means every public field already has the one-line description §13
// requires. Neither is a coincidence this script created.

/// The files whose `pub struct`/`pub enum` declarations make up the scenario surface.
///
/// The first is the schema itself. The rest are the crates whose types the schema
/// embeds: a scenario field typed `v2xw_core::weather::WeatherKind` publishes that
/// enum's variants, and it publishes them from that enum rather than from a copy, so a
/// variant added upstream appears in the form without anyone editing this list.
const REFLECT_SOURCES: [&str; 6] = [
    "src/scenario/schema.rs",
    "../v2xw-core/src/card.rs",
    "../v2xw-core/src/weather.rs",
    "../v2xw-world/src/lib.rs",
    "../v2xw-world/src/model.rs",
    "../v2xw-world/src/osm.rs",
];

/// One field of a reflected struct or struct-like enum variant.
#[derive(Default)]
struct RField {
    rust_name: String,
    wire_name: String,
    ty: String,
    doc: String,
    has_default: bool,
    flatten: bool,
    with: String,
}

/// One variant of a reflected enum.
#[derive(Default)]
struct RVariant {
    wire_name: String,
    doc: String,
    fields: Vec<RField>,
}

/// One reflected `pub struct` or `pub enum`.
#[derive(Default)]
struct RItem {
    name: String,
    doc: String,
    is_enum: bool,
    /// The internal tag, from `#[serde(tag = "…")]`; empty for an externally tagged or
    /// unit-only enum.
    tag: String,
    /// True when `#[serde(deny_unknown_fields)]` is on the container, which is what makes
    /// "the loader accepts exactly these keys" a statement the schema can make.
    deny_unknown: bool,
    fields: Vec<RField>,
    variants: Vec<RVariant>,
}

/// `SumoGerman` -> `sumo-german`, `Clear` -> `clear`, under one of serde's policies.
fn rename_all(name: &str, policy: &str) -> String {
    match policy {
        "lowercase" => name.to_lowercase(),
        "kebab-case" | "snake_case" => {
            let sep = if policy == "kebab-case" { '-' } else { '_' };
            let mut out = String::new();
            for (i, ch) in name.chars().enumerate() {
                if ch.is_uppercase() {
                    if i > 0 {
                        out.push(sep);
                    }
                    out.extend(ch.to_lowercase());
                } else {
                    out.push(ch);
                }
            }
            out
        }
        // An unrecognised policy is reported as "no policy" rather than guessed at: a
        // wrong wire name is a form control bound to a key the loader rejects.
        _ => name.to_string(),
    }
}

/// The value of `key = "…"` inside a run of attribute lines, if present.
fn attr_str(attrs: &[String], key: &str) -> Option<String> {
    let needle = format!("{key} = \"");
    for a in attrs {
        if let Some(at) = a.find(&needle) {
            let rest = &a[at + needle.len()..];
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

/// Whether a bare word appears as a serde attribute option (`default`, `flatten`, …).
fn attr_flag(attrs: &[String], key: &str) -> bool {
    for a in attrs {
        if !a.contains("serde") {
            continue;
        }
        for token in a.split(['(', ')', ',']) {
            if token.trim() == key {
                return true;
            }
        }
        // `default = "…"` is also a default.
        if a.contains(&format!("{key} = \"")) {
            return true;
        }
    }
    false
}

/// `pub name: Type,` -> `("name", "Type")`.
fn split_field(line: &str) -> Option<(String, String)> {
    let body = line.trim().strip_prefix("pub ")?;
    let colon = body.find(':')?;
    let name = body[..colon].trim().to_string();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let ty = body[colon + 1..].trim().trim_end_matches(',').trim();
    if ty.is_empty() {
        return None;
    }
    Some((name, ty.to_string()))
}

/// Joins a run of `///` lines into one paragraph.
///
/// The published `description` is one line a non-specialist can read, so the first
/// sentence-block is kept and the rest is dropped at a blank doc line: the paragraphs
/// after it are for someone reading the source, and a form field's help text that runs
/// to twenty lines is help nobody reads.
fn join_doc(docs: &[String]) -> String {
    let mut out = String::new();
    for line in docs {
        let t = line.trim();
        if t.is_empty() {
            if !out.is_empty() {
                break;
            }
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t);
    }
    // Intra-doc link syntax is for rustdoc; a form shows the words.
    out.replace("[`", "`").replace("`]", "`")
}

/// Reads every top-level `pub struct` and `pub enum` out of one source file.
///
/// Column zero means "top level" and four spaces means "a field of the item above",
/// which is `rustfmt`'s output and is checked by CI's format gate.
fn reflect_file(text: &str) -> Vec<RItem> {
    let lines: Vec<&str> = text.lines().collect();
    let mut items = Vec::new();
    let mut docs: Vec<String> = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        // Only column zero is an item, a doc comment for one, or an attribute on one.
        if raw.starts_with(char::is_whitespace) || raw.trim().is_empty() {
            if raw.trim().is_empty() {
                // A blank line inside a doc run is a paragraph break, which `join_doc`
                // uses; a blank line after an item's attributes cannot happen in
                // formatted code, so nothing is reset here.
                if !docs.is_empty() {
                    docs.push(String::new());
                }
            }
            i += 1;
            continue;
        }
        if let Some(d) = raw.strip_prefix("///") {
            docs.push(d.trim().to_string());
            i += 1;
            continue;
        }
        if raw.starts_with("#[") {
            // A `#[derive(…)]` may wrap onto several lines; gather to the closing `]`.
            let mut attr = raw.to_string();
            while !attr.trim_end().ends_with(']') && i + 1 < lines.len() {
                i += 1;
                attr.push(' ');
                attr.push_str(lines[i].trim());
            }
            attrs.push(attr);
            i += 1;
            continue;
        }
        if raw.starts_with("//!") || raw.starts_with("//") {
            i += 1;
            continue;
        }
        let is_struct = raw.starts_with("pub struct ") && raw.trim_end().ends_with('{');
        let is_enum = raw.starts_with("pub enum ") && raw.trim_end().ends_with('{');
        if !(is_struct || is_enum) {
            docs.clear();
            attrs.clear();
            i += 1;
            continue;
        }
        let head = raw
            .trim_end()
            .trim_end_matches('{')
            .trim()
            .strip_prefix(if is_struct { "pub struct" } else { "pub enum" })
            .unwrap_or("")
            .trim();
        // A generic item is skipped rather than half-reflected: the scenario has none,
        // and one would need a substitution this script does not do.
        let name = head.split(['<', ' ']).next().unwrap_or("").to_string();
        let policy = attr_str(&attrs, "rename_all").unwrap_or_default();
        let mut item = RItem {
            name,
            doc: join_doc(&docs),
            is_enum,
            tag: attr_str(&attrs, "tag").unwrap_or_default(),
            deny_unknown: attr_flag(&attrs, "deny_unknown_fields"),
            ..RItem::default()
        };
        docs.clear();
        attrs.clear();
        // A generic item cannot be published without substituting its parameters, so it
        // is left out rather than published wrong. The scenario has none.
        let include = !head.contains('<');

        // The body: to the closing brace at column zero.
        let mut fdocs: Vec<String> = Vec::new();
        let mut fattrs: Vec<String> = Vec::new();
        i += 1;
        while i < lines.len() && !lines[i].starts_with('}') {
            let line = lines[i];
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();
            if trimmed.is_empty() {
                if !fdocs.is_empty() {
                    fdocs.push(String::new());
                }
                i += 1;
                continue;
            }
            if indent != 4 {
                i += 1;
                continue;
            }
            if let Some(d) = trimmed.strip_prefix("///") {
                fdocs.push(d.trim().to_string());
                i += 1;
                continue;
            }
            if trimmed.starts_with("#[") {
                fattrs.push(trimmed.to_string());
                i += 1;
                continue;
            }
            if trimmed.starts_with("//") {
                i += 1;
                continue;
            }
            if is_enum {
                let vname = trimmed
                    .trim_end_matches(',')
                    .trim_end_matches('{')
                    .trim()
                    .split(['(', ' '])
                    .next()
                    .unwrap_or("")
                    .to_string();
                let wire = attr_str(&fattrs, "rename")
                    .unwrap_or_else(|| rename_all(&vname, &policy));
                let mut variant = RVariant {
                    wire_name: wire,
                    doc: join_doc(&fdocs),
                    fields: Vec::new(),
                };
                fdocs.clear();
                fattrs.clear();
                if trimmed.ends_with('{') {
                    // A struct-like variant: its fields are indented by eight.
                    let mut vdocs: Vec<String> = Vec::new();
                    let mut vattrs: Vec<String> = Vec::new();
                    i += 1;
                    while i < lines.len() && lines[i].trim() != "}," && lines[i].trim() != "}" {
                        let l = lines[i].trim();
                        if let Some(d) = l.strip_prefix("///") {
                            vdocs.push(d.trim().to_string());
                        } else if l.starts_with("#[") {
                            vattrs.push(l.to_string());
                        } else if let Some((fname, ty)) =
                            split_field(&format!("pub {}", l.trim_start_matches("pub ")))
                        {
                            variant.fields.push(RField {
                                wire_name: attr_str(&vattrs, "rename")
                                    .unwrap_or_else(|| fname.clone()),
                                rust_name: fname,
                                ty,
                                doc: join_doc(&vdocs),
                                has_default: attr_flag(&vattrs, "default"),
                                flatten: attr_flag(&vattrs, "flatten"),
                                with: attr_str(&vattrs, "with").unwrap_or_default(),
                            });
                            vdocs.clear();
                            vattrs.clear();
                        }
                        i += 1;
                    }
                }
                item.variants.push(variant);
                i += 1;
                continue;
            }
            if let Some((fname, ty)) = split_field(trimmed) {
                item.fields.push(RField {
                    wire_name: attr_str(&fattrs, "rename").unwrap_or_else(|| {
                        if policy.is_empty() {
                            fname.clone()
                        } else {
                            rename_all(&fname, &policy)
                        }
                    }),
                    rust_name: fname,
                    ty,
                    doc: join_doc(&fdocs),
                    has_default: attr_flag(&fattrs, "default"),
                    flatten: attr_flag(&fattrs, "flatten"),
                    with: attr_str(&fattrs, "with").unwrap_or_default(),
                });
            }
            fdocs.clear();
            fattrs.clear();
            i += 1;
        }
        if include && !item.name.is_empty() {
            items.push(item);
        }
    }
    items
}

/// Escapes a string for a Rust string literal.
fn lit(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Emits the reflected table as `$OUT_DIR/scenario_reflect.rs`.
fn emit_reflection() {
    let mut items: Vec<RItem> = Vec::new();
    for rel in REFLECT_SOURCES {
        println!("cargo:rerun-if-changed={rel}");
        let text = match std::fs::read_to_string(rel) {
            Ok(t) => t,
            // A source that cannot be read is a build error, not a smaller schema: a
            // silently short table would publish a form missing whole sections.
            Err(e) => panic!("cannot reflect {rel}: {e}"),
        };
        for item in reflect_file(&text) {
            if !items.iter().any(|existing| existing.name == item.name) {
                items.push(item);
            }
        }
    }

    let mut out = String::new();
    out.push_str("// @generated by build.rs from REFLECT_SOURCES. Do not edit.\n");
    out.push_str("pub(crate) static TYPES: &[RType] = &[\n");
    for it in &items {
        out.push_str("    RType {\n");
        out.push_str(&format!("        name: {},\n", lit(&it.name)));
        out.push_str(&format!("        doc: {},\n", lit(&it.doc)));
        out.push_str(&format!("        is_enum: {},\n", it.is_enum));
        out.push_str(&format!("        tag: {},\n", lit(&it.tag)));
        out.push_str(&format!("        deny_unknown: {},\n", it.deny_unknown));
        out.push_str("        fields: &[\n");
        for f in &it.fields {
            out.push_str(&emit_field(f));
        }
        out.push_str("        ],\n        variants: &[\n");
        for v in &it.variants {
            out.push_str("            RVariant {\n");
            out.push_str(&format!("                wire_name: {},\n", lit(&v.wire_name)));
            out.push_str(&format!("                doc: {},\n", lit(&v.doc)));
            out.push_str("                fields: &[\n");
            for f in &v.fields {
                out.push_str(&emit_field(f));
            }
            out.push_str("                ],\n            },\n");
        }
        out.push_str("        ],\n    },\n");
    }
    out.push_str("];\n");

    let dir = std::env::var("OUT_DIR").expect("OUT_DIR is set for a build script");
    let path = Path::new(&dir).join("scenario_reflect.rs");
    std::fs::write(&path, out).expect("writing the reflected scenario table");
}

/// One `RField` literal.
fn emit_field(f: &RField) -> String {
    format!(
        "                RField {{ rust_name: {}, wire_name: {}, ty: {}, doc: {}, \
         has_default: {}, flatten: {}, with: {} }},\n",
        lit(&f.rust_name),
        lit(&f.wire_name),
        lit(&f.ty),
        lit(&f.doc),
        f.has_default,
        f.flatten,
        lit(&f.with)
    )
}
