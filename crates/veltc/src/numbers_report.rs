//! `velt build --report numbers`: the `number` variables inside loops that the optimizer keeps
//! as doubles, each with the reason it could not store them as integers.

use velt_vir::vir;

/// The report for the optimized `program`, one line per variable, sorted by source position.
/// Variables of the standard library (inlined into user code) are left out.
pub fn render(program: &vir::Program) -> String {
    let slashes = |s: &str| s.replace('\\', "/");
    let std_root = crate::loader::std_root().map(|p| slashes(&p.to_string_lossy()));
    let in_std = |file: &str| {
        std_root
            .as_deref()
            .is_some_and(|r| slashes(file).starts_with(r))
    };
    let mut rows: Vec<(String, u32, u32, String)> = velt_opt::number_report(program)
        .into_iter()
        .flat_map(|(_, vars)| vars)
        .map(|u| {
            let (file, line, col) = match u.loc {
                Some(l) => (
                    program
                        .files
                        .get(l.file as usize)
                        .cloned()
                        .unwrap_or_default(),
                    l.line,
                    l.col,
                ),
                None => (String::new(), 0, 0),
            };
            (file, line, col, format!("`{}`: {}", u.name, u.reason))
        })
        .filter(|(file, ..)| !in_std(file))
        .collect();
    rows.sort();
    rows.dedup();
    let mut out = format!(
        "numbers: {} `number` variable{} in loops stay{} doubles\n",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        if rows.len() == 1 { "s" } else { "" },
    );
    for (file, line, col, what) in rows {
        if line == 0 {
            out.push_str(&format!("  {what}\n"));
        } else {
            out.push_str(&format!("  {file}:{line}:{col}: {what}\n"));
        }
    }
    out
}
