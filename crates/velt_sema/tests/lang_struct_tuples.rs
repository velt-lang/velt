//! A generic struct's tuple check (`struct_tuples`) follows what is instantiated, so a tuple
//! near a struct stays legal where no struct ends up holding it (#641). What it refuses is in
//! `tests/golden/lang/errors/struct_tuple_fields_generic.velt`.

mod common;

use common::programs::ok_src;

const PRELUDE: &str = "struct G<T> {
  v: T;
  n: number;
}
type P3 = [number, number, number];
";

fn ok(body: &str) {
    ok_src(&format!("{PRELUDE}{body}"));
}

#[test]
fn a_deferred_type_counts_for_what_it_reduces_to() {
    ok(
        "type TT<T> = T extends number ? [number, number, number] : string;
        function c<T>(x: TT<T>): number {
          const g: G<TT<T>> = { v: x, n: 1 };
          return g.n;
        }
        function main() {
          console.log(c<P3>(\"a\"));
        }",
    );
}

#[test]
fn a_tuple_on_a_parameter_no_field_stores_is_legal() {
    ok("class A<T, U> {
          k: T | null = null;
          m(x: U): U {
            const g: G<U> = { v: x, n: 1 };
            return g.v;
          }
        }
        function main() {
          console.log(new A<P3, number>().m(1));
        }");
}

#[test]
fn a_method_that_is_not_dispatched_does_not_count() {
    ok("class Cell<T> {
          v: T;
          constructor(v: T) {
            this.v = v;
          }
          get(): T {
            return this.v;
          }
          boxed(): G<T> {
            return { v: this.v, n: 1 };
          }
        }
        function main() {
          const p: P3 = [1, 2, 3];
          console.log(new Cell<P3>(p).get()[0]);
        }");
}

/// A mentioned type has no table for an `extend`'s interface, as a class has its vtable: its
/// method runs only where it is called, here on numbers.
#[test]
fn an_extend_counts_only_where_its_method_is_called() {
    ok("interface Extended<X> {
          e(x: X): X;
        }
        extend<T> Array<T> implements Extended<T> {
          e(x: T): T {
            const g: G<T> = { v: x, n: 1 };
            return g.v;
          }
        }
        function main() {
          const p: P3 = [1, 2, 3];
          const ps: P3[] = [p];
          const ns: number[] = [1];
          const h: Extended<number> = ns;
          console.log(ps.length, h.e(1));
        }");
}

/// With no interface arguments to tell instances apart, the conversion says which one runs:
/// std's arrays of tuples, which no call here converts, don't count.
#[test]
fn an_extend_counts_for_the_type_converted_to_its_interface() {
    ok("interface Ex {
          e(): number;
        }
        extend<T> Array<T> implements Ex {
          e(): number {
            const g: G<T> = { v: this[0], n: 1 };
            return g.n;
          }
        }
        function main() {
          const ns: number[] = [1];
          const h: Ex = ns;
          console.log(h.e());
        }");
}

/// A mentioned class has its vtable, but an `extend`'s interface isn't in it.
#[test]
fn an_extend_on_a_class_that_is_only_mentioned_does_not_count() {
    ok("class Bx<T> {
          v: T | null = null;
        }
        interface Ex<X> {
          e(x: X): X;
        }
        extend<T> Bx<T> implements Ex<T> {
          e(x: T): T {
            const g: G<T> = { v: x, n: 1 };
            return g.v;
          }
        }
        function main() {
          console.log(new Bx<P3>().v == null);
        }");
}

/// `f<P3>` doesn't lower the branch for primitives, where the struct is (hir_encodings.md).
#[test]
fn a_primitive_guard_keeps_an_object_argument_out() {
    ok("function isNum(x: unknown): x is number {
          return typeof x == \"number\";
        }
        function st<T>(x: T): number {
          const g: G<T> = { v: x, n: 1 };
          return g.n;
        }
        function f<T>(x: T): number {
          if (isNum(x)) {
            return st<T>(x);
          }
          return 0;
        }
        function main() {
          const p: P3 = [1, 2, 3];
          console.log(f<P3>(p), f<number>(1));
        }");
}

/// A guarded recursion is legal though its argument grows at each call (`instantiation_cycles`),
/// and `Blank`, which `BTreeMap` calls, is implemented by a blanket `extend`: carrying the call
/// must end, with and without a struct to carry.
#[test]
fn a_growing_guarded_recursion_ends() {
    let body = |store: &str| {
        format!(
            "import {{ BTreeMap }} from \"velt/collections\";
             function isNum(x: unknown): x is number {{
               return typeof x == \"number\";
             }}
             class Bx<T> {{
               v: T;
               constructor(v: T) {{
                 this.v = v;
               }}
             }}
             function f<T>(x: T): number {{
               if (isNum(x)) {{
                 return f<Bx<T>>(new Bx<T>(x));
               }}
               {store}
               const m = new BTreeMap<number, T>();
               m.set(1, x);
               return Number(m.size);
             }}
             function main() {{
               console.log(f<number>(1));
             }}"
        )
    };
    ok(&body(""));
    ok(&body("const g: G<T> = { v: x, n: 1 };"));
}
