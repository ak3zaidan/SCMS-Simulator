A scenario file is YAML or JSON and states everything a run needs: the seed, the clock,
the world, the actors, the radio and network stacks, the message sets, the security
configuration, the hardware profiles, the threats, the detectors, the metrics and the
exporters. Nothing else is an input — there are no environment variables, no hidden
defaults file and no wall-clock reads, which is what makes a run reproducible from the
file alone.

The reference below is extracted from the loader's own Rust types, so it is the schema
the engine enforces rather than a description of it. A worked, heavily commented
example is [`scenarios/phase1-manhattan.yaml`](scenarios/phase1-manhattan.yaml); it is
the file to copy.
