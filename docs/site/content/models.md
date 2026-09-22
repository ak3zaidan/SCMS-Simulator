Everything below is generated from the model cards the engine registers at start-up. It
is not a description of the models; it is the models' own declaration of themselves,
serialised. If a default changes in the code, it changes here on the next build, and if
it does not appear here it is not a parameter the engine reads.

How to read an entry:

- **Tier** says which fidelity levels the model implements. A scenario picks one tier
  per family; see [methodology](methodology.html) for what each tier leaves out.
- **Equations** are what the code computes, written as the implementer states them. They
  are there so a reader can check the code against the mathematics rather than against
  the prose.
- **Parameters** carry a unit, a default and a *source*. A source of "Not yet cited"
  means the number is an implementer's choice; those are collected on the
  [calibration debt](calibration.html) page with their plans.
- **What this tier ignores** is the most useful field on the card, and the one to read
  before quoting a result.
- The **card content hash** at the foot of each entry is what a run manifest pins. A
  replay refuses to run against a different hash unless explicitly allowed, and that
  allowance is itself recorded.
