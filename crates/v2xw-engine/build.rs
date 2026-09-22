//! Records the compiler and the target in the binary, for the run manifest.
//!
//! 02-architecture.md §6.5 and Phase 1 acceptance criterion 6 require the manifest to
//! carry the compiler version and the platform triple. Neither is knowable from inside a
//! running program without spawning a process, and the manifest must be reproducible, so
//! they are captured here — at build time, by the build itself — and embedded.
//!
//! This is not a wall-clock read: nothing here is a time, and the values are a property of
//! the build, which is exactly what the manifest is pinning. The one thing deliberately
//! *not* captured is a build timestamp; `Manifest::build_utc` is caller-supplied and
//! excluded from every digest for that reason.

use std::process::Command;

fn main() {
    // Re-run only when the toolchain pin or this script changes. Without these the script
    // reruns on every source edit, which costs a `rustc -vV` per build for no new
    // information.
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

    // The commit, when the build environment supplies one. CI sets it; a developer build
    // usually does not, and "unknown" is the honest answer there.
    let commit = std::env::var("V2XW_GIT_COMMIT").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=V2XW_GIT_COMMIT={commit}");
}
