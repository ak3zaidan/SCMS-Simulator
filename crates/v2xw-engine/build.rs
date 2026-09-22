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
}
