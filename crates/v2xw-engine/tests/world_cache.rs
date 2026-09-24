//! `world.cache`: an imported world is kept and reused, and the reuse is exact.
//!
//! The server builds a fresh kernel for every run, and before this key was wired every one
//! of them imported the world again — for the Manhattan extract, the longest part of
//! pressing Run. These tests hold the cache to the determinism contract: a cached world is
//! the *same* world (same content hash), the key changes when anything that decides the
//! import changes, and a damaged entry is repaired rather than trusted.

use std::path::{Path, PathBuf};

use v2xw_engine::Scenario;
use v2xw_engine::wiring::{build_world, world_cache_key};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

fn grid(cache: &Path) -> Scenario {
    let mut s = Scenario::load(repo_root().join("scenarios/phase1-grid.yaml")).expect("load");
    s.world.cache = Some(cache.display().to_string());
    s
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("v2xw-world-cache-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_cached_world_is_the_same_world() {
    let dir = scratch("same");
    let scenario = grid(&dir);
    let imported = build_world(&scenario).expect("import");
    let entry = dir.join(format!(
        "{}.v2xwworld",
        world_cache_key(&scenario).expect("key")
    ));
    assert!(entry.exists(), "the import was written to the cache");
    let cached = build_world(&scenario).expect("from the cache");
    assert_eq!(
        v2xw_world::hash::content_hash_hex(&imported),
        v2xw_world::hash::content_hash_hex(&cached),
        "the cached world must hash exactly as the imported one"
    );
}

#[test]
fn the_cache_is_read_and_not_merely_written() {
    // Put a *different* valid world under this scenario's key: if the cache is consulted,
    // that is the world that comes back. A cache that only ever wrote would pass the test
    // above and fail this one.
    let dir = scratch("read");
    let scenario = grid(&dir);
    let key = world_cache_key(&scenario).expect("key");
    let mut other = scenario.clone();
    other.world.cache = None;
    if let v2xw_world::WorldSourceSpec::Procedural { params, .. } = &mut other.world.source {
        params["cols"] = serde_json::json!(3);
        params["rows"] = serde_json::json!(3);
    }
    let decoy = build_world(&other).expect("decoy");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join(format!("{key}.v2xwworld")),
        v2xw_world::serde_native::to_bytes(&decoy).expect("bytes"),
    )
    .expect("write decoy");
    let got = build_world(&scenario).expect("from the cache");
    assert_eq!(
        v2xw_world::hash::content_hash_hex(&got),
        v2xw_world::hash::content_hash_hex(&decoy)
    );
}

#[test]
fn a_changed_world_section_is_a_different_key_and_a_damaged_entry_is_repaired() {
    let dir = scratch("key");
    let scenario = grid(&dir);
    let key = world_cache_key(&scenario).expect("key");
    let mut moved = scenario.clone();
    moved.world.imported_at = "2026-09-19T00:00:00Z".to_string();
    assert_ne!(
        key,
        world_cache_key(&moved).expect("key"),
        "any world input moves the key"
    );
    let mut elsewhere = scenario.clone();
    elsewhere.world.cache = Some("/somewhere/else".to_string());
    assert_eq!(
        key,
        world_cache_key(&elsewhere).expect("key"),
        "where the cache lives is not an input"
    );

    std::fs::create_dir_all(&dir).expect("dir");
    let entry = dir.join(format!("{key}.v2xwworld"));
    std::fs::write(&entry, b"not a world").expect("write garbage");
    let world = build_world(&scenario).expect("a damaged entry is not an error");
    let reread = v2xw_world::serde_native::from_bytes(&std::fs::read(&entry).expect("read"))
        .expect("the entry was rewritten");
    assert_eq!(
        v2xw_world::hash::content_hash_hex(&world),
        v2xw_world::hash::content_hash_hex(&reread)
    );
}
