//! The fundamental-diagram validation of 04-models.md §2.9, run at full size.
//!
//! ```text
//! cargo run --release -p v2xw-mobility --example fundamental_diagram
//! ```
//!
//! Loads a one-kilometre single-lane ring to sixteen densities from 5 to 140 veh/km, twice
//! at each density — once from an undisturbed homogeneous start and once from a deliberate
//! jam — measures flow and speed in one-minute bins with Edie's definitions, fits the
//! triangular diagram, and prints every measured quantity against its §2.9 target band.
//!
//! The crate's own test runs a smaller configuration of the same measurement
//! (`v2xw_mobility::fd::tests::the_ring_reproduces_the_fundamental_diagram_targets`); this
//! example is the one whose numbers are quotable, because it is the one that runs long
//! enough for the congested branch to be well sampled.

use v2xw_mobility::fd::{Branch, FdParams, measure};

fn main() {
    let params = FdParams::default();
    println!(
        "ring {:.0} m, {} densities, warm-up {:.0} s, measurement {:.0} s, bins of {:.0} s, \
         parameter set {:?}",
        params.ring.circumference_m,
        params.densities_veh_km.len(),
        params.warmup.as_secs_f64(),
        params.measure.as_secs_f64(),
        params.bin.as_secs_f64(),
        params.preset,
    );
    let result = measure(&params).expect("the measurement runs");

    println!("\n--- flow-density scatter (one-minute bins) ---");
    println!(
        "{:>10}  {:>8}  {:>12}  {:>10}  {:>6}",
        "branch", "k veh/km", "q veh/h", "v m/s", "N"
    );
    for bin in &result.bins {
        println!(
            "{:>10}  {:>8.1}  {:>12.1}  {:>10.2}  {:>6}",
            bin.branch.label(),
            bin.density_veh_km,
            bin.flow_veh_h,
            bin.speed_mps,
            bin.vehicles
        );
    }

    println!("\n--- fitted diagram ---");
    println!(
        "capacity                 {:>10.1} veh/h/lane",
        result.capacity_veh_h
    );
    println!(
        "critical density         {:>10.1} veh/km",
        result.critical_density_veh_km
    );
    println!(
        "capacity-drop density    {:>10.1} veh/km",
        result.capacity_drop_density_veh_km
    );
    println!(
        "free flow there          {:>10.1} veh/h/lane",
        result.free_flow_at_drop_veh_h
    );
    println!(
        "dynamic capacity there   {:>10.1} veh/h/lane",
        result.dynamic_capacity_veh_h
    );
    println!(
        "capacity drop            {:>10.2} %   (recomputed {:.2} %)",
        100.0 * result.capacity_drop,
        100.0 * result.capacity_drop_recomputed()
    );
    println!(
        "jam density              {:>10.1} veh/km",
        result.jam_density_veh_km
    );
    println!(
        "wave speed               {:>10.2} km/h",
        result.wave_speed_kmh
    );
    println!(
        "free speed               {:>10.2} m/s",
        result.free_speed_mps
    );
    println!("congested bins fitted    {:>10}", result.congested_bins);

    println!("\n--- what the parameters predict (arithmetic, not simulation) ---");
    println!(
        "Kesting 2010 Eq. 4.1     {:>10.1} veh/h/lane",
        result.theoretical_capacity_veh_h
    );
    println!(
        "1000/(l + s0)            {:>10.1} veh/km",
        result.theoretical_jam_density_veh_km
    );
    println!(
        "−(l + s0)/T              {:>10.2} km/h",
        result.theoretical_wave_speed_kmh
    );

    println!("\n--- against the 04-models.md §2.9 targets ---");
    let mut all_in_band = true;
    for (name, measured, target, ok) in result.report() {
        all_in_band &= ok;
        println!(
            "{name:<24} {measured:>14}   target {target:<12} {}",
            if ok { "IN BAND" } else { "OUT OF BAND" }
        );
    }
    println!("\n--- what this measurement does NOT cover (04-models.md §2.9) ---");
    for row in v2xw_mobility::fd::targets::uncovered_rows() {
        println!("- {row}");
    }

    println!(
        "\nvalidation.status = {}",
        if all_in_band {
            "literature-checked"
        } else {
            "unit-tested (a run outside every band marks the set unit-tested, §2.9)"
        }
    );
    let free = result
        .bins
        .iter()
        .filter(|b| b.branch == Branch::Free)
        .count();
    println!(
        "{free} free-branch bins and {} jammed-branch bins measured",
        result.bins.len() - free
    );
}
