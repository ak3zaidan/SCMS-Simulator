The roadmap's last phase states one release gate in one line: *no `todo-calibrate` on a
`high`-tier default without a calibration issue*. This page is that gate's verdict, run
over the registry the engine actually builds.

**What the gate adds to the [calibration-debt page](calibration.html).** That page lists
every uncited default and the plan its author wrote for replacing it. The card schema
already refuses an uncited default without a plan, so that list is complete and every row
on it has a sentence attached. This page asks the next question, which is the one a
release has to answer: *has anybody agreed to do it?* An issue in
`docs/calibration/issues.json` has an id, an owner, a state and the measurement that would
close it. A plan with no owner has never once been executed.

**It is red, and the register is empty.** Nobody has been assigned a calibration
measurement, so every uncited `high`-tier default in the registry fails. The eleven
shipped hardware profiles alone declare 141 fields with no published value, and every one
of those numbers sets the compute load of every run that uses the profile. Filling the
register with unowned issues would turn this page green without a single measurement being
made, which is exactly the failure the gate exists to prevent.

**Why the gate cannot be opened with one line.** A coverage pattern names one model by its
literal id. `*::*` does not parse, and a pattern that does not parse is itself a failure
rather than a line that is quietly ignored. The recurring defect this project keeps
finding in itself is a check that cannot go red, so the gate's own test suite injects the
wildcard and asserts the gate stays red.
