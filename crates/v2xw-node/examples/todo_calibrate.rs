//! Prints the registry's `todo-calibrate` page for the shipped hardware profiles
//! (03-interfaces.md §12 rule R1): every field that carries no published value.

fn main() {
    let profiles = v2xw_node::profiles::all();
    let mut total_fields = 0usize;
    for p in profiles {
        let report = p.field_report();
        total_fields += report.len();
        println!(
            "{:40} {:3}/{:3} uncalibrated",
            p.id,
            p.todo_calibrate_count(),
            report.len()
        );
    }
    println!(
        "\n{} profiles, {} modelled fields, {} uncalibrated",
        profiles.len(),
        total_fields,
        v2xw_node::profiles::todo_calibrate_total()
    );
}
