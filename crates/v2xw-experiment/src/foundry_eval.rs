//! Does a proposed mutation operator actually beat random search?
//!
//! The foundry's only pluggable component is the mutation operator
//! ([`crate::foundry::MutationOperator`]): parent selection, the feasibility oracle, the
//! validity gate, the fitness and the insert-if-better rule are identical whichever
//! operator is in the loop. This module measures whether swapping one in produces a
//! **better archive** — more coverage, a higher quality-diversity score, or a harder
//! hardest elite — than the built-in random operator.
//!
//! It is a port of `legacy/gui/foundry_eval.py`, which compared a language-model semantic
//! operator against random search. That harness lived on the GUI side because it imported
//! the language-model transport and the foundry must never import a user interface; here
//! the operator is a trait object, so the comparison needs nothing but the trait and this
//! module has no dependency on any particular kind of operator. A language-model operator
//! implements [`crate::foundry::MutationOperator`] in whatever crate owns its transport,
//! and this harness never learns what it is.
//!
//! ```no_run
//! use v2xw_experiment::foundry::{Objective, RandomMutation};
//! use v2xw_experiment::foundry_eval::{EvalOptions, compare_operators, render_report};
//! # use v2xw_experiment::runner::RunExecutor;
//! # use v2xw_experiment::foundry::MutationOperator;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let executor: &dyn RunExecutor = unimplemented!();
//! # let proposed: &dyn MutationOperator = unimplemented!();
//! let base = v2xw_engine::Scenario::load("scenarios/revocation-latency.yaml")?;
//! let options = EvalOptions::new("runs/foundry-eval")
//!     .budget(20)
//!     .seeds(vec![3, 7, 11])
//!     .objective(Objective::Evade);
//! let result = compare_operators(&base, executor, &RandomMutation, proposed, &options)?;
//! print!("{}", render_report(&result));
//! # Ok(()) }
//! ```
//!
//! # The comparison is fair, and this is what makes it fair
//!
//! For each seed the foundry is run **twice** with the same `(budget, seed, objective,
//! base genomes, base scenario)`. The only difference between the two runs is how the next
//! genome is proposed, so any gap isolates the operator's contribution. Both sides'
//! candidates are validated, scored and inserted by the same code.
//!
//! # Honesty rules this module keeps
//!
//! 1. **A null or negative result is reported plainly.** [`render_report`] says "no
//!    advantage detected" in as many words, and [`Comparison::verdict`] returns
//!    [`Verdict::NoAdvantage`]. Nothing here fabricates a win.
//! 2. **An operator that consults external state makes its side non-deterministic**, so
//!    the numbers are reported as a *mean over seeds* and the per-seed table is printed
//!    beside them. A single-seed comparison of a non-deterministic operator is an anecdote,
//!    and [`Comparison::caveat`] says so on the report when there are fewer than three.
//! 3. **The two sides are named**, not labelled "random" and "llm", because the harness
//!    does not know what the second one is and should not pretend to.
//! 4. **Coverage, QD-score and the hardest elite are reported separately** and a mixed
//!    result is called mixed. They measure different things: an operator can fill more
//!    cells while finding a weaker worst case, and averaging the three into one score
//!    would hide exactly that.
//!
//! # Cost
//!
//! Two foundry searches per seed. At a budget of 20 and three seeds that is 120 engine
//! runs plus six base-genome evaluations, **serially**. Start at a budget of 5 and one
//! seed to check the plumbing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ExperimentError, Result};
use crate::foundry::{Archive, FoundryOptions, MutationOperator, Objective, grid_size, search};
use crate::runner::RunExecutor;

/// The schema id `comparison.json` carries.
pub const COMPARISON_SCHEMA: &str = "v2xw/experiment-foundry-comparison/1";

/// The file the comparison document is written to.
pub const COMPARISON_FILE: &str = "comparison.json";

/// The file the human-readable verdict is written to.
pub const REPORT_FILE: &str = "REPORT.md";

/// The grid every reported float sits on, matching the archive's own.
pub const Q_EVAL: f64 = crate::foundry::Q_FITNESS;

/// How many seeds a comparison of a non-deterministic operator needs before its mean is
/// worth quoting.
///
/// Three. It is not a statistical threshold — three is far too few for that — it is the
/// smallest number from which a *spread* is visible at all, and the report says so rather
/// than implying the mean is an estimate.
pub const MIN_SEEDS_FOR_A_MEAN: usize = 3;

/// What one archive is worth, in the four numbers the comparison turns on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorMetrics {
    /// Filled descriptor cells.
    pub coverage: usize,
    /// The sum of the elites' fitnesses.
    pub qd_score: f64,
    /// The single hardest elite.
    pub best_fitness: f64,
    /// The mean elite fitness.
    pub mean_fitness: f64,
    /// The coverage denominator.
    pub grid_size: usize,
    /// How many candidates were evaluated, so a reader can see the budget was matched.
    pub evaluated: u64,
    /// How many candidates failed to run.
    pub failures: u64,
    /// The filled cells, by key, in key order — the targeted-gap-filling evidence.
    pub cells: Vec<String>,
}

/// Reduces one archive to its comparison metrics.
#[must_use]
pub fn archive_metrics(archive: &Archive) -> OperatorMetrics {
    let fitnesses: Vec<f64> = archive.cells.values().map(|e| e.fitness).collect();
    let best = fitnesses.iter().copied().fold(0.0_f64, f64::max);
    let mean = if fitnesses.is_empty() {
        0.0
    } else {
        // Ordered, so the mean is a function of the multiset and not of the cell order.
        v2xw_core::math::sum_ordered(fitnesses.iter().copied()) / fitnesses.len() as f64
    };
    OperatorMetrics {
        coverage: archive.coverage(),
        qd_score: archive.qd_score(),
        best_fitness: quantise(best),
        mean_fitness: quantise(mean),
        grid_size: grid_size(),
        evaluated: archive.evaluated,
        failures: archive.failures,
        cells: archive.cells.keys().cloned().collect(),
    }
}

/// One seed's head-to-head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeedResult {
    /// The seed both sides ran under.
    pub seed: u64,
    /// The first operator's archive.
    pub a: OperatorMetrics,
    /// The second operator's archive.
    pub b: OperatorMetrics,
    /// `b.coverage − a.coverage`.
    pub delta_coverage: i64,
    /// `b.qd_score − a.qd_score`.
    pub delta_qd: f64,
    /// `b.best_fitness − a.best_fitness`.
    pub delta_best: f64,
    /// Cells only the second operator filled — where it went that the first did not.
    pub b_only_cells: Vec<String>,
    /// Cells only the first operator filled.
    pub a_only_cells: Vec<String>,
}

/// What the comparison concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// The second operator wins on coverage, QD-score **and** the hardest elite.
    Wins,
    /// It wins on some of the three and not others.
    Mixed,
    /// It wins on none of the three. A valid result, reported as one.
    NoAdvantage,
}

/// The whole comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    /// The schema id, [`COMPARISON_SCHEMA`].
    pub schema: String,
    /// The objective both sides maximised.
    pub objective: String,
    /// The budget each side had, per seed.
    pub budget: u64,
    /// The first operator's name.
    pub operator_a: String,
    /// The second operator's name.
    pub operator_b: String,
    /// Per seed, in the order the seeds were given.
    pub per_seed: Vec<SeedResult>,
    /// The first operator's metrics, averaged over seeds.
    pub mean_a: MeanMetrics,
    /// The second operator's metrics, averaged over seeds.
    pub mean_b: MeanMetrics,
    /// Whether the second operator filled more cells on average.
    pub wins_coverage: bool,
    /// Whether it scored higher on average.
    pub wins_qd: bool,
    /// Whether it found a harder hardest elite on average.
    pub wins_best: bool,
}

/// The four numbers averaged over seeds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeanMetrics {
    /// Mean filled cells.
    pub coverage: f64,
    /// Mean QD-score.
    pub qd_score: f64,
    /// Mean hardest elite.
    pub best_fitness: f64,
    /// Mean elite fitness.
    pub mean_fitness: f64,
}

impl Comparison {
    /// What the comparison concluded.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        match (self.wins_coverage, self.wins_qd, self.wins_best) {
            (true, true, true) => Verdict::Wins,
            (false, false, false) => Verdict::NoAdvantage,
            _ => Verdict::Mixed,
        }
    }

    /// How many seeds the comparison ran.
    #[must_use]
    pub fn seeds(&self) -> usize {
        self.per_seed.len()
    }

    /// The caveat a reader needs before quoting the mean, or `None` when there is none.
    #[must_use]
    pub fn caveat(&self) -> Option<String> {
        if self.seeds() < MIN_SEEDS_FOR_A_MEAN {
            Some(format!(
                "Only {} seed(s). An operator that consults external state is \
                 non-deterministic, so a mean over fewer than {MIN_SEEDS_FOR_A_MEAN} \
                 seeds is an anecdote rather than an estimate. Do not quote this as a \
                 result.",
                self.seeds()
            ))
        } else {
            None
        }
    }

    /// Writes `comparison.json` into `dir`.
    ///
    /// # Errors
    /// [`ExperimentError::Io`] if the file cannot be written and [`ExperimentError::Json`]
    /// if the document will not serialise.
    pub fn write(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| ExperimentError::io("cannot create the comparison directory", dir, e))?;
        let path = dir.join(COMPARISON_FILE);
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| ExperimentError::json("the operator comparison", e))?;
        std::fs::write(&path, &bytes)
            .map_err(|e| ExperimentError::io("cannot write the comparison", &path, e))?;
        let report = dir.join(REPORT_FILE);
        std::fs::write(&report, render_report(self).as_bytes())
            .map_err(|e| ExperimentError::io("cannot write the comparison report", &report, e))?;
        Ok(())
    }
}

/// How a comparison is run.
#[derive(Debug, Clone)]
pub struct EvalOptions {
    /// Where `comparison.json` and `REPORT.md` go. Each side's own archive goes into a
    /// subdirectory named for the operator and the seed, so both are inspectable
    /// afterwards.
    pub out: PathBuf,
    /// The budget each side gets, per seed. Matched by construction.
    pub budget: u64,
    /// The objective both sides maximise.
    pub objective: Objective,
    /// The seeds to run. Both sides run every one of them.
    pub seeds: Vec<u64>,
    /// How many retries a mutation gets before falling back.
    pub max_tries: u32,
}

impl EvalOptions {
    /// The defaults: a budget of twenty, the `evade` objective, three seeds.
    #[must_use]
    pub fn new(out: impl Into<PathBuf>) -> EvalOptions {
        EvalOptions {
            out: out.into(),
            budget: 20,
            objective: Objective::Evade,
            seeds: vec![3, 7, 11],
            max_tries: 24,
        }
    }

    /// The same options with another budget.
    #[must_use]
    pub fn budget(mut self, budget: u64) -> Self {
        self.budget = budget;
        self
    }

    /// The same options with another objective.
    #[must_use]
    pub fn objective(mut self, objective: Objective) -> Self {
        self.objective = objective;
        self
    }

    /// The same options with another seed list.
    #[must_use]
    pub fn seeds(mut self, seeds: Vec<u64>) -> Self {
        self.seeds = seeds;
        self
    }

    /// The foundry options one side runs under for one seed.
    ///
    /// **Both sides get this same function**, differing only in the output directory, so
    /// there is no route by which one side could be given a different budget, objective or
    /// base-genome set. That is the whole methodological content of this module and it is
    /// worth one function to make it structural rather than careful.
    #[must_use]
    pub fn side(&self, operator: &str, seed: u64) -> FoundryOptions {
        let mut options = FoundryOptions::new(self.out.join(format!("{operator}-seed{seed}")));
        options.budget = self.budget;
        options.objective = self.objective.clone();
        options.seed = seed;
        options.max_tries = self.max_tries;
        options
    }
}

/// Runs both operators over every seed and returns the verdict.
///
/// # Errors
/// Whatever a foundry search failed with. A search that produces an *empty archive* is not
/// an error — it is a result, and a comparison of two empty archives reports no advantage,
/// which is the honest answer.
pub fn compare_operators<E, A, B>(
    base: &v2xw_engine::Scenario,
    executor: &E,
    operator_a: &A,
    operator_b: &B,
    options: &EvalOptions,
) -> Result<Comparison>
where
    E: RunExecutor + ?Sized,
    A: MutationOperator + ?Sized,
    B: MutationOperator + ?Sized,
{
    let mut per_seed = Vec::with_capacity(options.seeds.len());
    for &seed in &options.seeds {
        let archive_a = search(
            base,
            executor,
            operator_a,
            &options.side(operator_a.name(), seed),
        )?;
        let archive_b = search(
            base,
            executor,
            operator_b,
            &options.side(operator_b.name(), seed),
        )?;
        let a = archive_metrics(&archive_a);
        let b = archive_metrics(&archive_b);
        per_seed.push(SeedResult {
            seed,
            delta_coverage: b.coverage as i64 - a.coverage as i64,
            delta_qd: quantise(b.qd_score - a.qd_score),
            delta_best: quantise(b.best_fitness - a.best_fitness),
            b_only_cells: difference(&b.cells, &a.cells),
            a_only_cells: difference(&a.cells, &b.cells),
            a,
            b,
        });
    }

    let mean_a = mean_of(&per_seed, |r| &r.a);
    let mean_b = mean_of(&per_seed, |r| &r.b);
    let comparison = Comparison {
        schema: COMPARISON_SCHEMA.to_string(),
        objective: options.objective.label(),
        budget: options.budget,
        operator_a: operator_a.name().to_string(),
        operator_b: operator_b.name().to_string(),
        wins_coverage: mean_b.coverage > mean_a.coverage,
        wins_qd: mean_b.qd_score > mean_a.qd_score,
        wins_best: mean_b.best_fitness > mean_a.best_fitness,
        mean_a,
        mean_b,
        per_seed,
    };
    comparison.write(&options.out)?;
    Ok(comparison)
}

/// The cells in `left` that are not in `right`. Both are key-ordered, so the result is too.
fn difference(left: &[String], right: &[String]) -> Vec<String> {
    left.iter()
        .filter(|c| !right.contains(c))
        .cloned()
        .collect()
}

/// The mean of one side's metrics over the seeds.
fn mean_of(per_seed: &[SeedResult], pick: fn(&SeedResult) -> &OperatorMetrics) -> MeanMetrics {
    if per_seed.is_empty() {
        return MeanMetrics {
            coverage: 0.0,
            qd_score: 0.0,
            best_fitness: 0.0,
            mean_fitness: 0.0,
        };
    }
    let n = per_seed.len() as f64;
    let mean = |f: fn(&OperatorMetrics) -> f64| {
        quantise(v2xw_core::math::sum_ordered(per_seed.iter().map(|r| f(pick(r)))) / n)
    };
    MeanMetrics {
        coverage: mean(|m| m.coverage as f64),
        qd_score: mean(|m| m.qd_score),
        best_fitness: mean(|m| m.best_fitness),
        mean_fitness: mean(|m| m.mean_fitness),
    }
}

/// A float on the comparison's declared grid.
fn quantise(x: f64) -> f64 {
    v2xw_core::math::quantize_to(x, Q_EVAL)
}

/// Renders the comparison as Markdown.
#[must_use]
pub fn render_report(result: &Comparison) -> String {
    let a = &result.operator_a;
    let b = &result.operator_b;
    let yes = |won: bool| if won { "YES" } else { "NO" };

    let mut out = String::new();
    out.push_str(&format!(
        "# Foundry operator evaluation: `{b}` against `{a}`\n\n"
    ));
    out.push_str(&format!(
        "Does `{b}` beat `{a}` in the foundry's MAP-Elites loop? Both sides ran the **same \
         budget ({}), the same seeds, the same base scenario, the same base genomes and \
         the same objective (`{}`)** — the only difference is how the next genome is \
         proposed, so every gap below isolates the operator.\n\n",
        result.budget, result.objective
    ));
    if let Some(caveat) = result.caveat() {
        out.push_str(&format!("> **{caveat}**\n\n"));
    }
    out.push_str(&format!(
        "- Seeds: **{}**  ·  Budget per side per seed: **{}**  ·  Coverage denominator: \
         **{} cells**\n\n",
        result.seeds(),
        result.budget,
        grid_size()
    ));

    out.push_str("## Per seed\n\n");
    out.push_str(&format!(
        "| seed | cov ({a}) | cov ({b}) | Δcov | QD ({a}) | QD ({b}) | ΔQD | best ({a}) | \
         best ({b}) | Δbest | runs ({a}) | runs ({b}) |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n"
    ));
    for r in &result.per_seed {
        out.push_str(&format!(
            "| {} | {} | {} | {:+} | {} | {} | {:+} | {} | {} | {:+} | {} | {} |\n",
            r.seed,
            r.a.coverage,
            r.b.coverage,
            r.delta_coverage,
            r.a.qd_score,
            r.b.qd_score,
            r.delta_qd,
            r.a.best_fitness,
            r.b.best_fitness,
            r.delta_best,
            r.a.evaluated,
            r.b.evaluated,
        ));
    }
    out.push('\n');

    // The budget check, stated rather than assumed: if the two sides evaluated different
    // numbers of candidates the comparison is not matched, whatever the options said.
    let mismatched: Vec<&SeedResult> = result
        .per_seed
        .iter()
        .filter(|r| r.a.evaluated != r.b.evaluated)
        .collect();
    if !mismatched.is_empty() {
        out.push_str(&format!(
            "> **The budgets were not matched on {} seed(s).** The two `runs` columns \
             above differ, which means one side evaluated more candidates than the other \
             and the comparison is not fair. Treat the verdict below as void and find out \
             why before rerunning.\n\n",
            mismatched.len()
        ));
    }

    out.push_str("## Mean across seeds\n\n");
    out.push_str("| operator | coverage | QD-score | hardest elite | mean elite |\n");
    out.push_str("|---|---:|---:|---:|---:|\n");
    out.push_str(&format!(
        "| {a} | {} | {} | {} | {} |\n",
        result.mean_a.coverage,
        result.mean_a.qd_score,
        result.mean_a.best_fitness,
        result.mean_a.mean_fitness
    ));
    out.push_str(&format!(
        "| {b} | {} | {} | {} | {} |\n\n",
        result.mean_b.coverage,
        result.mean_b.qd_score,
        result.mean_b.best_fitness,
        result.mean_b.mean_fitness
    ));

    out.push_str("## Verdict\n\n");
    out.push_str(&format!(
        "- **Coverage** (more descriptor cells filled): {} — {b} {} against {a} {}.\n",
        yes(result.wins_coverage),
        result.mean_b.coverage,
        result.mean_a.coverage
    ));
    out.push_str(&format!(
        "- **QD-score** (sum of elite fitnesses): {} — {b} {} against {a} {}.\n",
        yes(result.wins_qd),
        result.mean_b.qd_score,
        result.mean_a.qd_score
    ));
    out.push_str(&format!(
        "- **Hardest elite** (the most evasive scenario found): {} — {b} {} against {a} \
         {}.\n\n",
        yes(result.wins_best),
        result.mean_b.best_fitness,
        result.mean_a.best_fitness
    ));
    match result.verdict() {
        Verdict::Wins => out.push_str(&format!(
            "**`{b}` beats `{a}` on coverage, QD-score and the hardest elite at this \
             budget.** Because both sides shared the budget, the seeds, the scenario and \
             the objective, the operator is the cause of the gap.\n\n"
        )),
        Verdict::Mixed => {
            let won: Vec<&str> = [
                ("coverage", result.wins_coverage),
                ("QD-score", result.wins_qd),
                ("the hardest elite", result.wins_best),
            ]
            .iter()
            .filter(|(_, w)| *w)
            .map(|(n, _)| *n)
            .collect();
            let lost: Vec<&str> = [
                ("coverage", result.wins_coverage),
                ("QD-score", result.wins_qd),
                ("the hardest elite", result.wins_best),
            ]
            .iter()
            .filter(|(_, w)| !*w)
            .map(|(n, _)| *n)
            .collect();
            out.push_str(&format!(
                "**Mixed result:** `{b}` wins on {} and not on {}. The three are not \
                 interchangeable — an operator can fill more cells while finding a weaker \
                 worst case — so they are not averaged into one score.\n\n",
                won.join(", "),
                lost.join(", ")
            ));
        }
        Verdict::NoAdvantage => out.push_str(&format!(
            "**No advantage detected:** `{b}` does not beat `{a}` on coverage, QD-score \
             or the hardest elite at this budget. This is a valid result and is reported \
             as one. A larger budget or more seeds may change it; this harness does not \
             fabricate a win, and an operator that costs an API call per candidate has to \
             earn its place here before it earns a place in a study.\n\n"
        )),
    }

    out.push_str("## Where each operator went that the other did not\n\n");
    let mut any = false;
    for r in &result.per_seed {
        if r.a_only_cells.is_empty() && r.b_only_cells.is_empty() {
            continue;
        }
        any = true;
        out.push_str(&format!("**Seed {}**\n\n", r.seed));
        if !r.b_only_cells.is_empty() {
            out.push_str(&format!(
                "- only `{b}`: {}\n",
                r.b_only_cells
                    .iter()
                    .map(|c| format!("`{c}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !r.a_only_cells.is_empty() {
            out.push_str(&format!(
                "- only `{a}`: {}\n",
                r.a_only_cells
                    .iter()
                    .map(|c| format!("`{c}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push('\n');
    }
    if !any {
        out.push_str("Both operators filled the same cells on every seed.\n\n");
    }
    out.push_str(
        "This is the targeted-gap-filling evidence: a coverage number says *how many* \
         cells an operator reached and this says *which*. An operator that reads the \
         archive summary should be filling cells the random one missed, and if it is not, \
         it is not using what it was given.\n\n",
    );

    out.push_str("## Method, and its limits\n\n");
    out.push_str(&format!(
        "- Both sides run the **same** foundry code: only \
         `MutationOperator::propose` differs. Parent selection, the feasibility oracle, \
         the validity gate, the fitness and the insert-if-better rule are shared.\n\
         - Every candidate's engine seed is derived from `(seed, candidate index)` by \
         SHA-256, so the candidate seeds are matched between the two sides as well.\n\
         - `{a}`'s side is deterministic given its seed. An operator that consults \
         external state is not, which is why the numbers are a mean over seeds.\n\
         - The three numbers are reported separately and a mixed result is called mixed.\n\
         - **The archives themselves are on disk**, one subdirectory per operator per \
         seed, so this verdict can be checked against them rather than taken on trust.\n"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundry::{Descriptor, Elite, Genome, Signals};

    fn signals() -> Signals {
        Signals {
            attackers: 2,
            reported_subjects: 3,
            recall_vehicle: Some(0.5),
            recall_report: Some(0.5),
            time_to_detect_s: None,
        }
    }

    fn elite(family: &str, fitness: f64) -> Elite {
        Elite {
            descriptor: Descriptor {
                attack_family: family.to_string(),
                density_band: "medium".to_string(),
                topology: "procedural".to_string(),
                attacker_band: "low".to_string(),
                rat: "dsrc-80211p".to_string(),
                protocol: "none".to_string(),
            },
            fitness,
            candidate: 1,
            seed_hex: "0x0000000000000001".to_string(),
            scenario_hash: "hash".to_string(),
            genome: Genome::new(),
            signals: signals(),
        }
    }

    fn archive(cells: &[(&str, f64)], evaluated: u64) -> Archive {
        let mut archive = Archive::new();
        for (family, fitness) in cells {
            archive.insert_if_better(elite(family, *fitness));
        }
        archive.evaluated = evaluated;
        archive
    }

    fn comparison(a: Archive, b: Archive) -> Comparison {
        let a = archive_metrics(&a);
        let b = archive_metrics(&b);
        let per_seed = vec![SeedResult {
            seed: 7,
            delta_coverage: b.coverage as i64 - a.coverage as i64,
            delta_qd: quantise(b.qd_score - a.qd_score),
            delta_best: quantise(b.best_fitness - a.best_fitness),
            b_only_cells: difference(&b.cells, &a.cells),
            a_only_cells: difference(&a.cells, &b.cells),
            a,
            b,
        }];
        let mean_a = mean_of(&per_seed, |r| &r.a);
        let mean_b = mean_of(&per_seed, |r| &r.b);
        Comparison {
            schema: COMPARISON_SCHEMA.to_string(),
            objective: "evade".to_string(),
            budget: 20,
            operator_a: "random".to_string(),
            operator_b: "proposed".to_string(),
            wins_coverage: mean_b.coverage > mean_a.coverage,
            wins_qd: mean_b.qd_score > mean_a.qd_score,
            wins_best: mean_b.best_fitness > mean_a.best_fitness,
            mean_a,
            mean_b,
            per_seed,
        }
    }

    #[test]
    fn an_empty_archive_reduces_to_zeros_rather_than_a_division() {
        let metrics = archive_metrics(&Archive::new());
        assert_eq!(metrics.coverage, 0);
        assert_eq!(metrics.qd_score, 0.0);
        assert_eq!(metrics.best_fitness, 0.0);
        assert_eq!(metrics.mean_fitness, 0.0, "not NaN");
        assert!(metrics.cells.is_empty());
    }

    #[test]
    fn the_metrics_are_the_four_numbers_they_claim_to_be() {
        let metrics = archive_metrics(&archive(&[("position", 0.25), ("stealth", 0.75)], 10));
        assert_eq!(metrics.coverage, 2);
        assert_eq!(metrics.qd_score, 1.0);
        assert_eq!(metrics.best_fitness, 0.75);
        assert_eq!(metrics.mean_fitness, 0.5);
        assert_eq!(metrics.evaluated, 10);
    }

    /// The honesty rule, as an assertion.
    ///
    /// **Shown to fail:** changing [`Comparison::verdict`]'s `(false, false, false)` arm
    /// to `Verdict::Mixed` makes this go red. A harness that had no way to say "no" would
    /// be a harness that always says yes.
    #[test]
    fn a_null_result_is_reported_as_a_null_result() {
        let result = comparison(
            archive(&[("position", 0.9), ("stealth", 0.9)], 20),
            archive(&[("position", 0.1)], 20),
        );
        assert_eq!(result.verdict(), Verdict::NoAdvantage);
        assert!(!result.wins_coverage && !result.wins_qd && !result.wins_best);
        let report = render_report(&result);
        assert!(report.contains("No advantage detected"));
        assert!(!report.contains("beats `random` on coverage"));
    }

    #[test]
    fn a_clean_win_is_reported_as_one() {
        let result = comparison(
            archive(&[("position", 0.1)], 20),
            archive(&[("position", 0.9), ("stealth", 0.5)], 20),
        );
        assert_eq!(result.verdict(), Verdict::Wins);
        let report = render_report(&result);
        assert!(report.contains("beats `random`"));
        assert!(report.contains("the operator is the cause of the gap"));
    }

    #[test]
    fn a_mixed_result_is_called_mixed_and_names_both_halves() {
        // More cells, weaker best: exactly the case an averaged score would hide.
        let result = comparison(
            archive(&[("position", 0.9)], 20),
            archive(&[("position", 0.5), ("stealth", 0.5)], 20),
        );
        assert_eq!(result.verdict(), Verdict::Mixed);
        assert!(result.wins_coverage);
        assert!(!result.wins_best);
        let report = render_report(&result);
        assert!(report.contains("Mixed result"));
        assert!(report.contains("coverage"));
        assert!(report.contains("the hardest elite"));
    }

    #[test]
    fn the_cell_difference_is_the_targeted_gap_evidence() {
        let result = comparison(
            archive(&[("position", 0.5)], 20),
            archive(&[("stealth", 0.5)], 20),
        );
        let seed = &result.per_seed[0];
        assert_eq!(seed.a_only_cells.len(), 1);
        assert_eq!(seed.b_only_cells.len(), 1);
        assert!(seed.a_only_cells[0].starts_with("position|"));
        assert!(seed.b_only_cells[0].starts_with("stealth|"));
        let report = render_report(&result);
        assert!(report.contains("only `proposed`"));
        assert!(report.contains("only `random`"));
    }

    #[test]
    fn a_thin_comparison_carries_its_caveat() {
        let result = comparison(archive(&[], 20), archive(&[], 20));
        assert_eq!(result.seeds(), 1);
        assert!(result.caveat().is_some());
        assert!(render_report(&result).contains("anecdote"));
    }

    /// An unmatched budget voids the comparison, and the report says so.
    ///
    /// This is the check that would catch a bug in the harness itself: if one side's
    /// search stopped early, its coverage would be lower for a reason that has nothing to
    /// do with its operator.
    #[test]
    fn an_unmatched_budget_voids_the_verdict_on_the_report() {
        let result = comparison(
            archive(&[("position", 0.5)], 20),
            archive(&[("stealth", 0.9)], 12),
        );
        let report = render_report(&result);
        assert!(report.contains("budgets were not matched"));
        assert!(report.contains("void"));
    }

    #[test]
    fn both_sides_are_given_the_same_options_but_a_different_directory() {
        let options = EvalOptions::new("runs/eval")
            .budget(7)
            .seeds(vec![1])
            .objective(Objective::Latency);
        let a = options.side("random", 1);
        let b = options.side("proposed", 1);
        assert_eq!(a.budget, b.budget);
        assert_eq!(a.budget, 7);
        assert_eq!(a.objective, b.objective);
        assert_eq!(a.seed, b.seed);
        assert_eq!(a.max_tries, b.max_tries);
        assert_eq!(a.base_genomes, b.base_genomes);
        assert_ne!(a.out, b.out, "the two sides must not share a directory");
    }

    #[test]
    fn the_mean_is_a_mean_over_the_seeds() {
        let mut result = comparison(archive(&[("position", 0.2)], 20), archive(&[], 20));
        // A second seed with a different answer, so the mean is doing arithmetic.
        let second = SeedResult {
            seed: 11,
            a: archive_metrics(&archive(&[("position", 0.8)], 20)),
            b: archive_metrics(&archive(&[], 20)),
            delta_coverage: -1,
            delta_qd: -0.8,
            delta_best: -0.8,
            b_only_cells: Vec::new(),
            a_only_cells: vec!["x".to_string()],
        };
        result.per_seed.push(second);
        let mean = mean_of(&result.per_seed, |r| &r.a);
        assert_eq!(mean.coverage, 1.0);
        assert_eq!(mean.qd_score, 0.5, "(0.2 + 0.8) / 2");
        assert_eq!(mean.best_fitness, 0.5);
    }
}
