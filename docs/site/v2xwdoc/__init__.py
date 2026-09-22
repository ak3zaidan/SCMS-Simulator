"""The V2X World Simulator documentation-site generator.

Standard library only, on purpose: the site has to build on any machine that can clone
the repository, years from now, without a package index being reachable.

Modules
-------
* `md` — the Markdown subset and the `{% include %}` directive.
* `render` — the page shell, the navigation and the table helpers.
* `cards` — the model reference, the calibration page and the validation page, all
  generated from the engine's model-card dump.
* `scenario` — the scenario schema reference, extracted from the loader's Rust types.
* `findings` — the defect register, parsed from `docs/design/findings/`.
* `site` — which pages exist and what each is built from.
"""

__version__ = "0.1.0"
