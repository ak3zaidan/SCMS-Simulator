# V2X World Simulator — task runner (ADR 0010 §2/§3).
# `just --list` shows every recipe. Recipes marked TODO are placeholders for
# functionality that lands in a later phase of docs/design/10-roadmap.md.
#
# Cross-platform contract (design brief hard rule 4): every recipe below runs on
# macOS, Linux and Windows. Recipes use one command per line with no shell
# syntax, so they behave identically under sh and PowerShell; conditionals use
# just's own `path_exists` rather than a shell `if`. The handful of recipes that
# genuinely need a POSIX shell are marked `[unix]` and are Linux/macOS only.

set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# The committed map fixture the determinism gate runs on. Small, offline, stable.
fixture := "tests/fixtures/midtown-6block.osm.xml"

# Show the available recipes.
default:
    @just --list

# One-command setup: fetch every dependency the present trees need.
setup: setup-rust setup-python setup-ui
    @echo ">> setup complete"

[doc("Fetch Rust dependencies only")]
setup-rust:
    cargo fetch

[doc("Install Python dependencies, when that tree exists")]
setup-python:
    @just _setup-python-{{ if path_exists("python/pyproject.toml") == "true" { "yes" } else { "no" } }}
_setup-python-yes:
    uv sync --project python
_setup-python-no:
    @echo ">> skip python/: no pyproject.toml yet (v2xw-py lands in a later phase)"

[doc("Install UI dependencies, when that tree exists")]
setup-ui:
    @just _setup-ui-{{ if path_exists("ui/package.json") == "true" { "yes" } else { "no" } }}
_setup-ui-yes:
    pnpm --dir ui install
_setup-ui-no:
    @echo ">> skip ui/: no package.json yet"

# Build the whole workspace.
build:
    cargo build --workspace

# Type-check the workspace, tests and benches included.
check:
    cargo check --workspace --all-targets

# Run the Rust test suite.
test:
    cargo test --workspace

# Format the workspace in place.
fmt:
    cargo fmt --all

# Clippy with warnings denied — the same gate CI applies.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# The gate CI enforces: formatting, clippy, tests.
ci: fmt-check lint test

[doc("Fail if anything is unformatted (the check CI runs)")]
fmt-check:
    cargo fmt --all -- --check

# --- determinism (ADR 0004, Phase 1 acceptance criterion 1) -------------------

# Import the committed fixture and print this machine's canonical world hash.
#
# The hash is produced by the engine's own canonicalisation, so every platform
# must print the same value; CI compares all three and fails if they differ.
[doc("Print this machine's canonical world hash for the committed fixture")]
world-hash out="target/determinism":
    cargo run --release --quiet -p v2xw-world --example import_osm -- {{fixture}} {{out}} 2000-01-01T00:00:00Z

[doc("Import the full Manhattan extract (not committed; see worlds/cache/README.md)")]
import-manhattan out="worlds/cache/manhattan-import":
    cargo run --release --quiet -p v2xw-world --example import_osm -- worlds/cache/manhattan.osm.xml {{out}} 2000-01-01T00:00:00Z

# Reclaims several GB without discarding compiled dependencies.
[doc("Drop the incremental compile cache (safe when no build is in flight)")]
clean-incremental:
    cargo clean --profile dev -p v2xw-core -p v2xw-world

# --- run and serve ------------------------------------------------------------

# Run a scenario through the engine. TODO (Phase 1): awaiting v2xw-engine (D8).
run SCENARIO:
    @echo "TODO (Phase 1): 'cargo run -p v2xw-cli -- run {{SCENARIO}}' once v2xw-engine exists (build-decision D8)."

# Serve the Studio UI in development mode.
studio:
    pnpm --dir ui dev

# Build the Studio UI for production.
studio-build:
    pnpm --dir ui build

# Run the UI test suites.
ui-test:
    pnpm --dir ui test

# --- legacy reference (frozen Python engine, ADR 0002) ------------------------
# POSIX-only: these drive a venv whose interpreter path differs on Windows, and
# CI runs them on Linux alone because the vectors are platform independent.

[unix]
[doc("Create the frozen legacy Python engine's virtualenv")]
legacy-setup:
    uv venv --python 3.12 legacy/.venv
    uv pip install --python legacy/.venv -e "./legacy[test]"

[unix]
[doc("Run the legacy conformance vectors the new engine must reproduce")]
legacy-conformance:
    cd legacy && .venv/bin/python -m pytest tests/test_butterfly.py tests/test_linkage.py tests/test_leakage.py tests/test_ml_contract.py tests/test_dataset_integrity.py -q

[unix]
[doc("Run the full frozen legacy test suite")]
legacy-test:
    cd legacy && .venv/bin/python -m pytest tests -q

# Install SUMO (optional; enables the high mobility tier only). TODO (Phase 3).
sumo-install:
    @echo "TODO (Phase 3): SUMO is optional (ADR 0005). macOS: 'brew install --cask sumo-gui' or 'uv pip install eclipse-sumo libsumo'."
