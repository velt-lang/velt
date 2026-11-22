//! An assignment to an integer array's element used as a value (`element_value.rs`, #3453)
//! checks its target again; a target in error is reported once.

mod common;

use common::programs::err_src;

#[test]
fn a_readonly_element_assigned_as_a_value_is_reported_once() {
    let forms = ["ro[0] = x", "ro[0] += 3", "ro[0] += x"];
    for form in forms {
        let src = format!(
            "const ro: readonly u8[] = Buffer.alloc(1);\nconst x: any = 1;\nconst r = ({form});\nconsole.log(r);\n"
        );
        let e = err_src(&src);
        let n = e.matches("only permits reading").count();
        assert_eq!(n, 1, "`{form}` reports the readonly element once:\n{e}");
    }
}
