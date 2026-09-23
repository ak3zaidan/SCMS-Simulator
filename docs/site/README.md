# `docs/site/` — the documentation site

A static site generated from this repository: the model reference comes from the
engine's model cards, the scenario schema from the loader's Rust types, and the defect
register from `docs/design/findings/`. Nothing about a model is written twice, so
nothing about a model can drift.

```sh
just --justfile docs/site/justfile cards    # export the model cards (compiles Rust)
just --justfile docs/site/justfile build    # generate the site into docs/site/build/
just --justfile docs/site/justfile serve    # http://localhost:8000
```

Without `just`: `python3 docs/site/build.py --clean --out docs/site/build`. The
generator needs Python 3.8 or newer **and nothing else** — no package index, no network
access, no virtual environment.

To wire the recipes into the root justfile, add one line to it:

```just
mod site 'docs/site/justfile'
```

after which `just site build`, `just site check` and `just site cards` work from the
repository root. This directory does not edit the root justfile itself.

## Layout

| Path | What it is |
|---|---|
| `build.py` | the entry point: argument parsing and the build report |
| `v2xwdoc/md.py` | a Markdown subset and the `{% include %}` directive |
| `v2xwdoc/render.py` | the page shell, navigation and table helpers |
| `v2xwdoc/cards.py` | model reference, calibration debt and validation pages, from the card dump |
| `v2xwdoc/campaign.py` | the validation campaign report: the registry joined against the run output and the defect registers |
| `v2xwdoc/scenario.py` | the scenario schema reference, extracted from `schema.rs` |
| `v2xwdoc/findings.py` | the defect register, parsed from `docs/design/findings/` |
| `v2xwdoc/site.py` | which pages exist and what each is built from |
| `content/*.md` | the hand-written prose: overview, architecture, methodology, the plug-in tutorial, glossary, the honesty-page leads |
| `validation-runs.template.json` | the validation campaign's input schema, with every case `not-run` because nothing has measured them |
| `assets/site.css` | one stylesheet; the site loads no script and no web font |
| `tools/cardgen/` | the Rust exporter that dumps the engine's registry to JSON |
| `build/`, `generated/` | outputs, git-ignored |

## The validation campaign's second input

`campaign.html` joins three things: the card dump (what the code claims), the validation
suite's output (what was measured) and the defect registers (what was wrong anyway). The
second of those is `docs/site/generated/validation-runs.json`, whose schema is documented
at the top of `v2xwdoc/campaign.py` and whose shape is in
`validation-runs.template.json` beside this file.

**The template's rows are all `not-run`, and that is deliberate.** The cases are the ones
04-models.md §13 defines, with the targets and tolerances that document states; what has
not happened is the measurement. Putting an `observed` value in that nobody measured
would fabricate a validation result, which is precisely what the page exists to make
impossible.

A missing run document is reported, never skipped: the page then says it is reporting
claims and not measurements, and `--strict` fails. `--strict` **also** fails when a card
claims `literature-checked` or `field-checked` while no validation case names the model,
or while a case naming it failed — 04-models.md §13's rule that a failing case blocks its
models from being labelled checked, enforced rather than stated.

## The card dump

`tools/cardgen` builds a `v2xw_core::registry::Registry` through the crate-level
registration entry points and serialises every registration to
`generated/cards.json`. It has its own `[workspace]` table, so it is not a member of
the root workspace and `cargo build --workspace` is unaffected; the cost is a separate
target directory. Adding `docs/site/tools/cardgen` to the root workspace's members and
deleting that table is a two-line change if the owner prefers one target directory.

The dump's shape is `{schema, cardgen_version, engine_version, stages[], coverage,
model_count, models[]}`, where each model is `{id, licence, hosting, content_hash,
card}` and `card` is `ModelCard` exactly as `serde` writes it. The site reads only
those fields, so any other producer of that shape — a `v2xw cards export` subcommand,
for instance, which is where this logic would more naturally live — can replace the
exporter without touching the generator.

**Known coverage gap, stated in the output rather than hidden.** Four registration
entry points exist today: `v2xw_engine::wiring::register_all` (node and mobility),
`v2xw_sec::register_all`, `v2xw_proto::register_all` and
`v2xw_metrics::register_all`. The radio, message, network and threat crates publish
their cards through per-model constructors rather than a crate-level list, so their
models are not in the dump. The exporter writes that gap into `coverage.uncovered` and
the site prints it on the model reference. Closing it means either adding a
`model_cards()` function to each of those crates, in the shape `v2xw-mobility` already
has, or adding the exporter to the CLI where it can reach the models a scenario
actually builds.

## Conventions this generator keeps

- **No timestamps.** A build takes no clock reading, so the same tree produces the same
  bytes. `--stamp` adds a tree identifier to the footer when someone wants one.
- **Missing input is stated, never skipped.** No card dump means three pages that say
  why they are empty; `--strict` turns any such warning into a non-zero exit, which is
  what CI should use.
- **No severity filter.** The defect register ranks the severity labels it knows and
  shows the ones it does not rather than dropping them. Filtering findings against a
  hard-coded list of labels is how a report loses rows silently. The campaign page keeps
  the same rule for its `outcome` vocabulary.
- **Generated pages report disagreements, not summaries.** The campaign page's first
  table is the list of models whose card and whose measurements say different things. A
  page that only reported the cards would be true on the day it was written.
- **Includes, not copies.** A page quotes a design document by section, spliced in at
  build time with a line saying where it came from.

## Known issue found while writing this

`docs/design/findings/ui-review-register.md` contains a stray NUL byte at offset
34,859. It makes `grep` treat the file as binary and skip it without a word, so any
grep-driven search over the defect registers silently misses that file. The site reads
the registers as bytes and strips the NUL, so nothing is missing from the generated
page, but the byte should be removed at source. That file is not this directory's to
edit.
