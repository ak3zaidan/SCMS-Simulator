The site is built by one Python script with no dependencies outside the standard
library, and it takes no timestamp, so the same working tree produces byte-identical
output. That is the same property the engine promises for a run, applied to its
documentation: if two people build the site from the same commit and get different
bytes, something is wrong.

## Building it

```sh
just --justfile docs/site/justfile build     # writes docs/site/build/
just --justfile docs/site/justfile serve     # http://localhost:8000
```

Or without `just`:

```sh
python3 docs/site/build.py --clean --out docs/site/build
```

Continuous integration should run the strict form, which fails when an input is
missing rather than publishing a site with a hole in it:

```sh
just --justfile docs/site/justfile check
```

## Regenerating the model reference

Three pages — the model reference, the calibration debt page and the validation status
page — are generated from a dump of the engine's model registry. The dump is the one
step that compiles Rust:

```sh
just --justfile docs/site/justfile cards     # writes docs/site/generated/cards.json
just --justfile docs/site/justfile build
```

or both at once with `just --justfile docs/site/justfile all`.

If the dump is absent the site still builds, and those three pages say plainly that
they are empty because nobody ran the exporter. They do not silently render as though
the engine had no models.

## What is generated from what

| Page | Built from |
|---|---|
| Model reference, calibration debt, validation status | `docs/site/generated/cards.json`, produced by `docs/site/tools/cardgen` from the engine's registry |
| Scenario schema | `crates/v2xw-engine/src/scenario/schema.rs`, read as text: the structs, their doc comments, their `serde` attributes and their `Default` implementations |
| Defect register | every `.md` under `docs/design/findings/` |
| Architecture, methodology | hand-written prose plus sections included verbatim from `docs/design/` at build time |
| Overview, glossary, plug-in tutorial, this page | hand-written |

The include mechanism matters more than it looks. A page says
`{% include docs/design/02-architecture.md#7. Fidelity ladder %}` and the generator
splices that section in, with a line saying where it came from. Nothing is copied, so
nothing drifts, and there is exactly one place to edit.

## Adding a page

1. Write `docs/site/content/<name>.md`.
2. Add an entry to the `pages` list in `docs/site/v2xwdoc/site.py` and to `_nav()` in
   the same file.
3. If the page has a generated half, give the entry a `body` callable and a
   `generated_from` string, which is what prints the "this page is generated" banner.

The Markdown subset is documented at the top of `docs/site/v2xwdoc/md.py`. Two
departures from CommonMark are deliberate: underscores never mean emphasis, because
`snake_case` identifiers are everywhere in this repository's prose, and raw HTML is
escaped rather than passed through.

## Publishing

The output is a directory of static files with no JavaScript, no web fonts and no
absolute paths. It works from `file://`, from any static host, and from GitHub Pages.

**GitHub Pages.** Build on a push to `main` and upload `docs/site/build` as the Pages
artefact. The whole workflow is:

```yaml
name: docs
on:
  push:
    branches: [main]
permissions:
  contents: read
  pages: write
  id-token: write
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build the card dump
        run: cargo run --release --manifest-path docs/site/tools/cardgen/Cargo.toml -- --out docs/site/generated/cards.json
      - name: Build the site
        run: python3 docs/site/build.py --clean --strict --out docs/site/build
      - uses: actions/upload-pages-artifact@v3
        with:
          path: docs/site/build
  deploy:
    needs: build
    runs-on: ubuntu-latest
    environment:
      name: github-pages
    steps:
      - uses: actions/deploy-pages@v4
```

Two notes on that workflow. The card-dump step is the only one that compiles anything,
and it is also the only one that can go stale, so running it in the same job as the
build is what keeps the published model reference honest. And `--strict` is what stops
a site missing its model reference from being published as though it were complete.

**Anywhere else.** `rsync -a --delete docs/site/build/ <host>:<path>/` is the whole
deployment. There is no server-side component.

## Why not a documentation framework

ADR 0007 decision 3 says "docs are generated from cards (mkdocs)". The cards part is
the binding decision and is honoured exactly; the tool is not. A framework would have
brought a Python dependency tree that has to resolve years from now for the
documentation to build at all, and the interesting half of this site — the card
reference, the calibration debt, the schema extraction, the defect register — is custom
generation that a framework would not have provided anyway. What remained was a
Markdown renderer and a page template, which is about six hundred lines.

The deviation is recorded here rather than left implicit, which is the same rule the
build applies to the design documents: where implementation disproved the plan, the
document says so and says why.
