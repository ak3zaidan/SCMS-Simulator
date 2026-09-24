//! Build script: generate Rust ASN.1 bindings from the ETSI forge modules.
//!
//! Build decision D2 fixes the encoder strategy: the ETSI facilities layer and the
//! IEEE 1609.2 / TS 103 097 security envelope are **really encoded**, from bindings this
//! script generates with `rasn-compiler`; SAE J2735 is not, because `rasn-compiler`
//! cannot compile its `RegionalExtension` information-object-class idiom.
//!
//! Two independent generation units are produced into `OUT_DIR`:
//!
//! | Unit | File | ASN.1 modules | Encoding |
//! |---|---|---|---|
//! | facilities | `etsi_facilities.rs` | `ETSI-ITS-CDD` (TS 102 894-2 R2), `CAM-PDU-Descriptions` (TS 103 900 R2), `DENM-PDU-Description` (TS 103 831 R2) | UPER |
//! | security | `sec_types.rs` | `Ieee1609Dot2BaseTypes`, `Ieee1609Dot2`, `Ieee1609Dot2CrlBaseTypes`, `Ieee1609Dot2Crl`, `EtsiTs103097Module`, `EtsiTs103097ExtensionModule` | COER |
//!
//! They are separate because they are consumed separately: `v2xw-sec` needs the second
//! and nothing of the first, and the CDD (370 KB of ASN.1, ~10 k lines of Rust) has no
//! business being recompiled inside the security crate.
//!
//! # Hermetic and offline
//!
//! Every input is a file inside this repository, under `third_party/asn1/etsi/`. The
//! script opens no socket, spawns no process and reads no environment variable other
//! than the ones Cargo sets. If an input is missing the script says which file and where
//! it should have come from, rather than failing inside the compiler.
//!
//! # Encoding hygiene (D4)
//!
//! Three upstream modules (`ETSI-ITS-CDD`, `Ieee1609Dot2`, `Ieee1609Dot2BaseTypes`) are
//! CP1252, not UTF-8, and `rasn-compiler` aborts on them with "stream did not contain
//! valid UTF-8". The byte-exact originals are kept for provenance; this script reads the
//! `normalized-utf8/` copies, which differ only in comment characters. See
//! `third_party/asn1/etsi/PROVENANCE.md` for both hashes of each.
//!
//! # Patches (D5)
//!
//! `rasn-compiler` 0.16 has a known codegen defect in `DEFAULT` values of fixed-size
//! BIT STRINGs, which hits `Ieee1609Dot2`. It is carried as a recorded patch file in
//! `patches/`, applied to the generated text, and the build fails if a patch no longer
//! applies — so an upstream fix is noticed rather than silently ignored.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rasn_compiler::prelude::*;

/// One generation unit: an output file name and the ASN.1 sources that feed it.
struct Unit {
    /// File name written into `OUT_DIR` and pulled in with `include!`.
    out_file: &'static str,
    /// Human name used in diagnostics.
    label: &'static str,
    /// Source modules, **in dependency order** (imports before importers). The compiler
    /// links the whole set at once, but keeping the order meaningful makes the error
    /// message readable when one of them fails to parse.
    sources: &'static [&'static str],
}

/// The ETSI facilities layer: CAM and DENM over the Release 2 common data dictionary.
///
/// CAM is TS 103 900 (Release 2) rather than EN 302 637-2 (Release 1) because Release 2
/// is what the CDD in this tree belongs to: the Release 1 CAM imports `ITS-Container`,
/// a different dictionary module. Mixing the two would need both dictionaries and would
/// produce two incompatible spellings of every shared data element.
const FACILITIES: Unit = Unit {
    out_file: "etsi_facilities.rs",
    label: "ETSI facilities (CAM, DENM, VAM)",
    sources: &[
        // D4: the CDD is CP1252 upstream; generate from the normalized copy.
        "normalized-utf8/cdd_ts102894_2/ETSI-ITS-CDD.asn",
        "cam_ts103900/CAM-PDU-Descriptions.asn",
        "denm_ts103831/DENM-PDU-Descriptions.asn",
        // TS 103 300-3 V2.2.1, the VRU awareness message a pedestrian's device sends.
        "vam_ts103300_3/VAM-PDU-Descriptions.asn",
    ],
};

/// IEEE 1609.2 plus the ETSI TS 103 097 profile of it. Consumed by `v2xw-sec`.
const SECURITY: Unit = Unit {
    out_file: "sec_types.rs",
    label: "IEEE 1609.2 / ETSI TS 103 097 security types",
    sources: &[
        // D4: both 1609.2 modules are CP1252 upstream.
        "normalized-utf8/ieee1609.2/Ieee1609Dot2BaseTypes.asn",
        "normalized-utf8/ieee1609.2/Ieee1609Dot2.asn",
        "ieee1609.2/Ieee1609Dot2CrlBaseTypes.asn",
        "ieee1609.2/Ieee1609Dot2Crl.asn",
        "sec_ts103097/EtsiTs103097ExtensionModule.asn",
        "sec_ts103097/EtsiTs103097Module.asn",
    ],
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // A build script that panics buries the cause under a backtrace and a
            // "process didn't exit successfully". Print the diagnostic we actually want
            // the reader to act on, then fail quietly.
            eprintln!("\nv2xw-msg: ASN.1 code generation failed.\n{e}\n");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR")?);
    let out_dir = PathBuf::from(env("OUT_DIR")?);

    // <repo>/crates/v2xw-msg -> <repo>/third_party/asn1/etsi
    let asn_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            format!(
                "cannot locate the workspace root above {}",
                manifest_dir.display()
            )
        })?
        .join("third_party/asn1/etsi");

    if !asn_root.is_dir() {
        return Err(format!(
            "the committed ETSI ASN.1 tree is missing:\n  expected: {}\n\n\
             These modules are BSD-3-Clause (ETSI forge, forge.etsi.org/rep/ITS/asn1) and are\n\
             committed to this repository on purpose; see third_party/asn1/etsi/PROVENANCE.md\n\
             for the source URL, commit and sha256 of each file. Restore the directory from\n\
             git (`git checkout -- third_party/asn1`) rather than re-downloading it, so the\n\
             pinned revisions stay pinned.",
            asn_root.display()
        ));
    }

    let patches = load_patches(&manifest_dir.join("patches"))?;

    for unit in [FACILITIES, SECURITY] {
        generate(&unit, &asn_root, &out_dir, &patches)?;
    }

    // The generated text is a function of the sources and the patches only.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=patches");
    for unit in [FACILITIES, SECURITY] {
        for src in unit.sources {
            println!(
                "cargo:rerun-if-changed={}",
                asn_root.join(src).to_string_lossy()
            );
        }
    }
    Ok(())
}

fn env(key: &str) -> Result<String, String> {
    std::env::var(key).map_err(|_| format!("Cargo did not set {key}; this is not a Cargo build"))
}

/// Runs `rasn-compiler` over one unit's sources and writes the patched result.
fn generate(unit: &Unit, asn_root: &Path, out_dir: &Path, patches: &[Patch]) -> Result<(), String> {
    let mut paths = Vec::with_capacity(unit.sources.len());
    for src in unit.sources {
        let path = asn_root.join(src);
        if !path.is_file() {
            return Err(format!(
                "missing ASN.1 source for the {} bindings:\n  expected: {}\n\n\
                 It should have been committed with the rest of third_party/asn1/etsi.\n\
                 PROVENANCE.md in that directory records its upstream URL, commit and sha256.",
                unit.label,
                path.display()
            ));
        }
        paths.push(path);
    }

    // `Compiler` is a typestate: the first source moves it out of `CompilerMissingParams`,
    // and only then does it have `compile_to_string`. Hence the split rather than a fold.
    let (first, rest) = paths
        .split_first()
        .ok_or_else(|| format!("generation unit `{}` has no sources", unit.label))?;
    let mut compiler = Compiler::<RasnBackend, _>::new().add_asn_by_path(first);
    for path in rest {
        compiler = compiler.add_asn_by_path(path);
    }

    let result = compiler.compile_to_string().map_err(|e| {
        format!(
            "rasn-compiler could not turn the {} modules into Rust.\n\
             \n\
             Compiler said:\n  {e}\n\
             \n\
             Sources, in the order they were added:\n{}\n\
             What to check, in order:\n\
             \x20 1. The file named in the error is UTF-8. Three upstream modules are CP1252\n\
             \x20    (ETSI-ITS-CDD, Ieee1609Dot2, Ieee1609Dot2BaseTypes) and must be read from\n\
             \x20    third_party/asn1/etsi/normalized-utf8/ (build decision D4).\n\
             \x20 2. Every module the failing one IMPORTs is in this unit's source list.\n\
             \x20 3. The construct at the reported position is one rasn-compiler 0.16 supports.\n\
             \x20    Known gaps, recorded in D5: `WITH COMPONENTS` inner subtyping on a CHOICE\n\
             \x20    (ETSI TS 102 941 PKI), and parameterised types over an information object\n\
             \x20    class (SAE J2735 RegionalExtension). Neither is in this unit.\n\
             \x20 4. If a module was deliberately added, add it to the unit in build.rs *and*\n\
             \x20    to third_party/asn1/etsi/PROVENANCE.md, which is the licence record.",
            unit.label,
            unit.sources
                .iter()
                .map(|s| format!("\x20 - {s}\n"))
                .collect::<String>(),
        )
    })?;

    for warning in &result.warnings {
        println!("cargo:warning=v2xw-msg [{}]: {warning}", unit.label);
    }

    let mut generated = sanitise_doc_comments(&result.generated);
    apply_patches(unit, &mut generated, patches)?;

    let header = format!(
        "// @generated by crates/v2xw-msg/build.rs from the ETSI forge ASN.1 modules.\n\
         // Unit: {}. Do not edit: regenerate by touching build.rs.\n\
         // Sources and licences: third_party/asn1/etsi/PROVENANCE.md (BSD-3-Clause).\n\
         // Patches applied: see crates/v2xw-msg/patches/.\n",
        unit.label
    );

    let dest = out_dir.join(unit.out_file);
    std::fs::write(&dest, header + &generated)
        .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(())
}

/// Makes the ASN.1 comments safe to put in a Rust doc comment.
///
/// `rasn-compiler` copies each ASN.1 `/** … */` comment into `#[doc = "…"]`, and the ETSI
/// and IEEE modules write prose the way specifications do: nested bullet lists indented
/// four or more spaces, and fenced blocks around formulae. Markdown reads both as **code**,
/// and `cargo test` then tries to compile them as Rust — which is how a clean build grows
/// 170 failing doctests out of sentences like "- The psid field in P is equal to …".
///
/// Two substitutions fix it without losing the text:
///
/// * runs of two or more spaces become one, so no line is indented enough to start a code
///   block (Markdown collapses runs of spaces anyway, so nothing renders differently);
/// * ``` fences are removed, so a formula in the specification's prose stays prose.
///
/// Only the contents of `#[doc = "…"]` attributes are touched, and only spaces and
/// backticks within them, so no generated identifier, literal or expression can be
/// affected — the same transformation applied to code would be a bug, which is why the
/// match is anchored on the whole line.
fn sanitise_doc_comments(generated: &str) -> String {
    let mut out = String::with_capacity(generated.len());
    for line in generated.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("#[doc = \"")
            && let Some(content) = rest.strip_suffix("\"]")
        {
            let indent = &line[..line.len() - trimmed.len()];
            let mut cleaned = String::with_capacity(content.len());
            let mut spaces = 0usize;
            let mut chars = content.chars().peekable();
            while let Some(c) = chars.next() {
                // A `\t` in the generated source is an escaped tab, and a tab opens an
                // indented code block just as four spaces do. It arrives here as the two
                // characters `\` and `t`, so it is folded into the whitespace run rather
                // than passed through.
                if c == '\\' && matches!(chars.peek(), Some('t') | Some('n') | Some('r')) {
                    chars.next();
                    spaces += 1;
                    if spaces == 1 {
                        cleaned.push(' ');
                    }
                    continue;
                }
                if c == '\\' {
                    // Any other escape (`\"`, `\\`) is passed through untouched, both
                    // characters together, so the string literal stays well formed.
                    cleaned.push(c);
                    if let Some(next) = chars.next() {
                        cleaned.push(next);
                    }
                    spaces = 0;
                    continue;
                }
                if c == ' ' {
                    spaces += 1;
                    if spaces == 1 {
                        cleaned.push(' ');
                    }
                    continue;
                }
                spaces = 0;
                // Drop a ``` fence wherever it appears; a lone backtick is harmless and is
                // kept, because it is how the specifications mark up field names.
                if c == '`' && chars.peek() == Some(&'`') {
                    let mut lookahead = chars.clone();
                    lookahead.next();
                    if lookahead.next() == Some('`') {
                        chars.next();
                        chars.next();
                        continue;
                    }
                }
                cleaned.push(c);
            }
            out.push_str(indent);
            out.push_str("#[doc = \"");
            out.push_str(&cleaned);
            out.push_str("\"]");
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// A recorded fix for a known upstream defect.
///
/// The format is deliberately trivial — an exact string to find and what to put in its
/// place — because the thing being patched is *generated* text, where a context diff has
/// no stable line numbers to hang on. Each file states the defect it works around.
struct Patch {
    /// File name, used in diagnostics.
    name: String,
    /// Which generation unit this applies to, matched against `Unit::out_file`.
    applies_to: String,
    find: String,
    replace: String,
    /// How many occurrences must be found. A patch that stops applying fails the build.
    expect: usize,
}

/// Parses every `*.patch` in `dir`.
///
/// File format: `#` comments and `key: value` headers, then the two sections.
///
/// ```text
/// # why this exists
/// unit: sec_types.rs
/// expect: 1
/// @@FIND@@
/// <exact text>
/// @@REPLACE@@
/// <replacement>
/// @@END@@
/// ```
fn load_patches(dir: &Path) -> Result<Vec<Patch>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    // Sorted, so the applied order does not depend on the filesystem.
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?
    {
        let path = entry
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
            .path();
        if path.extension().is_some_and(|e| e == "patch") {
            files.insert(path);
        }
    }

    let mut patches = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        patches.push(parse_patch(&path, &text)?);
    }
    Ok(patches)
}

fn parse_patch(path: &Path, text: &str) -> Result<Patch, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bad = |what: &str| format!("{}: malformed patch file: {what}", path.display());

    let (head, rest) = text
        .split_once("@@FIND@@\n")
        .ok_or_else(|| bad("no @@FIND@@ section"))?;
    let (find, rest) = rest
        .split_once("@@REPLACE@@\n")
        .ok_or_else(|| bad("no @@REPLACE@@ section"))?;
    let (replace, _) = rest
        .split_once("@@END@@")
        .ok_or_else(|| bad("no @@END@@ marker"))?;

    let mut applies_to = None;
    let mut expect = 1usize;
    for line in head.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| bad(&format!("header line is not `key: value`: {line}")))?;
        match key.trim() {
            "unit" => applies_to = Some(value.trim().to_string()),
            "expect" => {
                expect = value
                    .trim()
                    .parse()
                    .map_err(|_| bad(&format!("`expect` is not a number: {value}")))?;
            }
            other => return Err(bad(&format!("unknown header `{other}`"))),
        }
    }

    Ok(Patch {
        name,
        applies_to: applies_to.ok_or_else(|| bad("missing `unit:` header"))?,
        // The section text ends with the newline that precedes the next marker; that
        // newline belongs to the marker, not to the payload.
        find: find.strip_suffix('\n').unwrap_or(find).to_string(),
        replace: replace.strip_suffix('\n').unwrap_or(replace).to_string(),
        expect,
    })
}

fn apply_patches(unit: &Unit, generated: &mut String, patches: &[Patch]) -> Result<(), String> {
    for patch in patches.iter().filter(|p| p.applies_to == unit.out_file) {
        let found = generated.matches(patch.find.as_str()).count();
        if found != patch.expect {
            let mut msg = String::new();
            let _ = write!(
                msg,
                "patch `{}` no longer applies to the generated {} bindings.\n\
                 It expected {} occurrence(s) of its FIND text and saw {found}.\n\
                 \n\
                 This patch works around a known rasn-compiler defect (the file says which).\n\
                 If rasn-compiler has fixed it, delete the patch file. If the defect moved,\n\
                 update the FIND text. Do not relax `expect` to 0 to make the build pass:\n\
                 the count is what tells us the workaround is still doing something.",
                patch.name, unit.label, patch.expect,
            );
            return Err(msg);
        }
        *generated = generated.replace(patch.find.as_str(), patch.replace.as_str());
        // Deliberately silent on success. `cargo:warning` is replayed on every build of the
        // workspace, so announcing a patch that applied cleanly would put a line of noise
        // in front of every `cargo test` run for the rest of the project's life. The list
        // that a reader or a manifest needs is `v2xw_msg::provenance::PATCHES`, and the
        // crate's own test checks it against this directory.
    }
    Ok(())
}
