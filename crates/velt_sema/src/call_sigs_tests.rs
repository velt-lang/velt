//! Interfaces with call signatures, and those extending a function type, are rewritten into
//! the function types they declare: the program's, never std's.

use crate::script_locals::sema_tests::programs;

/// A program's function type alias named like an interface a std interface extends (`Add`, which
/// `velt/ops`' `Arithmetic` extends) does not make that std interface a function type (#3057).
#[test]
fn a_program_s_function_type_name_leaves_std_s_interfaces_alone() {
    programs::ok_src(
        "type Add = (a: number, b: number) => number;\n\
         const plus: Add = (a, b) => a + b;\n\
         import type { Arithmetic } from \"velt/ops\";\n\
         function sum<T extends Arithmetic<T>>(xs: T[], zero: T): T {\n\
         \x20 let s = zero;\n\
         \x20 for (const x of xs) { s = s + x; }\n\
         \x20 return s;\n\
         }\n\
         function main() { console.log(sum([1, 2, 3], 0), plus(1, 2)); }\n",
    );
}

/// The program's own interface extending its function type alias still is a function with
/// properties (#921).
#[test]
fn a_program_s_interface_extending_its_function_type_is_callable() {
    programs::ok_src(
        "type EffectFn = () => void;\n\
         interface Subscriber extends EffectFn { deps: number }\n\
         function main() {\n\
         \x20 const s = (() => {}) as Subscriber;\n\
         \x20 s.deps = 1;\n\
         \x20 s();\n\
         }\n",
    );
}
