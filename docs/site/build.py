#!/usr/bin/env python3
"""Build the V2X World Simulator documentation site.

    python3 docs/site/build.py [--out docs/site/build] [--cards docs/site/generated/cards.json]

No dependencies beyond the standard library, no network access, no timestamps: the
same working tree produces byte-identical output, which is the same property the
engine itself promises for a run.

Exit status is 0 when every page was written. `--strict` additionally fails the build
when anything was missing -- the card dump, the schema source, a defect register --
which is what continuous integration should use, so that a site quietly missing its
model reference cannot be published as if it were complete.
"""

import argparse
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

from v2xwdoc import site  # noqa: E402  (path set above)

DEFAULT_OUT = os.path.join("docs", "site", "build")
DEFAULT_CARDS = os.path.join("docs", "site", "generated", "cards.json")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument(
        "--repo",
        default=os.path.abspath(os.path.join(HERE, "..", "..")),
        help="the repository root (default: two levels above this script)",
    )
    parser.add_argument(
        "--out", default=None, help="output directory (default: docs/site/build)"
    )
    parser.add_argument(
        "--cards",
        default=None,
        help="the model-card dump to read (default: docs/site/generated/cards.json)",
    )
    parser.add_argument(
        "--stamp",
        default="",
        help="an optional tree identifier to print in the footer, e.g. a commit hash. "
        "Left out by default so the build is reproducible.",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="exit non-zero if any input was missing or any stage failed",
    )
    parser.add_argument(
        "--clean",
        action="store_true",
        help="delete the output directory before building, so a page whose source was "
        "removed cannot survive in the published site",
    )
    args = parser.parse_args(argv)

    repo = os.path.abspath(args.repo)
    out = os.path.abspath(args.out or os.path.join(repo, DEFAULT_OUT))
    cards = os.path.abspath(args.cards or os.path.join(repo, DEFAULT_CARDS))

    if args.clean and os.path.isdir(out):
        shutil.rmtree(out)

    try:
        result = site.build(repo, HERE, out, cards, stamp=args.stamp)
    except Exception as failure:  # noqa: BLE001 -- the message matters, not the type
        # A build that cannot read one of its inputs fails loudly and says which one.
        # Rendering the rest of the site around the hole would publish a page that
        # looks complete and is not.
        sys.stderr.write("!! documentation build failed: " + str(failure) + "\n")
        return 2

    for note in result.notes:
        sys.stdout.write("   " + note + "\n")
    for warning in result.warnings:
        sys.stdout.write("!! " + warning + "\n")
    sys.stdout.write(
        ">> wrote "
        + str(len(result.pages))
        + " pages to "
        + os.path.relpath(out, repo)
        + "\n"
    )
    if args.strict and result.warnings:
        sys.stdout.write("!! --strict: failing because of the warnings above\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
