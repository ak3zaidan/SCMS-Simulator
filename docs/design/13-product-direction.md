# Product direction: the website is the simulator

Stated by the owner, 2026-09-23. This supersedes the implicit assumption in
`09-ui.md` that the command line is the primary surface and the browser is a
viewer. It is a repositioning, not a feature request, and it changes what
"finished" means for several components.

## 1. The website is the control panel, not a view onto one

> "I'm never going to access this through the terminal or sockets. That should be
> a backup, but the website should be the main focus."

Consequences:

- Anything the command line can do, the page must do. The command line remains
  supported for automation, continuous integration and headless runs, and it is
  the fallback when the page is broken, but it is no longer the reference
  interface.
- A capability that exists only as a command-line flag is an unfinished feature.
- `09-ui.md` should be read with this inverted.

## 2. Every configuration variable is editable from the page

> "All the configuration variables should be there. From amount of cars in the
> city, to how many packets get sent, to literally every single configuration
> variable that has to do with traffic or the network or the protocol."

**This must not be hand-written forms.** The scenario schema has around twenty
top-level sections with deep nesting, and the model registry holds every model's
parameters with their units, defaults, ranges and cited sources. Hand-written
forms would cover a subset on the day they were written and drift immediately.

**Decision: the settings interface is generated from the schema and the registry.**
The engine already publishes both. A field appears in the page because it exists
in the schema, not because somebody remembered it. That makes coverage total by
construction and makes drift impossible.

Requirements for it:

- Searchable. With this many parameters, search is the primary navigation, not a
  convenience.
- Grouped the way a user thinks: traffic, radio, network, security, threats,
  detection, measurement. Not grouped by crate.
- Every field shows its unit, its default, and its source or its
  todo-calibrate status. The explainability rule applies here more than anywhere:
  a user setting a parameter should see where the default came from.
- Validation inline, from the same validator the loader uses, so the page and the
  engine can never disagree about what is valid.
- A field the engine does not actually act on must not be shown. There is a
  documented case of six scenario keys being validated and hashed while never
  reaching the engine; a generated form would have exposed all six as editable.

## 3. Long runs are the target, not short ones

> "Run long range simulations that last for like minutes. We don't want to focus
> on like short 20 second simulations."

Consequences, which are not only interface work:

- The retained history window bounds how far back a run can be scrubbed; it is
  currently a step count chosen for short runs. Long runs need a policy that
  does not grow without bound and does not silently discard what the user wants
  to seek to.
- Memory across a run of minutes at useful fleet sizes has never been measured.
  The machine has 8 GB.
- Throughput is the binding constraint: measured 12.1 wall-clock seconds per
  simulated second at 997 vehicles. A five-minute run at that fleet size is an
  hour of wall time. Either the performance work lands or the page must be
  honest about the wait and let it proceed in the background.
- Pause, resume and stop must work mid-run and be responsive, not queue behind a
  long step.

## 4. Everything built in and usable from the page

Named explicitly: pseudonym rotation. More generally, a mechanism that exists in
a crate but cannot be switched on from the page is not usable, and the page is
where the switch belongs.

## 5. The bird's-eye view shows vehicles, and clicking one follows it

> "I should be able to see small dots or nodes which are vehicles moving around,
> and clicking on one should actually allow me to go into the follow view."

The map camera currently renders the network and the buildings but the vehicles
are not legible as objects at that zoom. The interaction the owner describes —
see the fleet from above, pick one, drop into its view — is the fly-down that
`09-ui.md` describes as the product's signature interaction, and it is the thing
a reviewer will judge the simulator by in the first ten seconds.

Requirements: a vehicle is a visible mark at map zoom regardless of distance, the
mark is picked by clicking, and the pick issues the follow that subscribes its
telemetry. The camera move between the two should be continuous rather than a
cut, because the point is that it is one world and one model.
