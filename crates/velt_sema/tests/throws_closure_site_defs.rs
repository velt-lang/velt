//! A closure checked against a function type whose error type is a site's variable throws into
//! that site, recorded by its definition (`throws::fn_flows`, `Ctx::note_closure_def`, #3033):
//! the later checks, which have no variables, find the site from it. The end-to-end case (an
//! overload's adapters sharing one span, sigx's `defineFactory`) needs #3038; until then this
//! counts the closures recorded. Each program is checked on its own, so the process-wide count
//! (`velt_sema::closure_site_defs`) is this program's: this file has one test.

mod common;

use common::programs::ok_src;

#[test]
fn a_closure_checked_against_a_site_variable_records_its_site() {
    let before = velt_sema::closure_site_defs();
    ok_src(
        r#"
class E1 extends Error {
  constructor(m: string) {
    super(m);
  }
}

type Fn<TArgs extends unknown[], R> = (...args: TArgs) => R;

function take(f: Fn<[string], number>): number {
  return f("x");
}

function main(): void {
  try {
    console.log(take((x: string): number => {
      throw new E1(x);
    }));
  } catch (e) {
    console.log("caught");
  }
}
"#,
    );
    assert!(velt_sema::closure_site_defs() > before);
}
