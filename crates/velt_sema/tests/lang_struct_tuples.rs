//! A generic struct's tuple check (`struct_tuples`) follows what is instantiated, so a tuple
//! near a struct stays legal where no struct ends up holding it, and where it can't tell, it
//! refuses and says what to write (#641). What it refuses is also in
//! `tests/golden/lang/errors/struct_tuple_fields_generic.velt`.

mod common;

use common::programs::{err_src, ok_src};

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

/// Nothing says what reaches it: the program has no tuple, and the error names what it wrote,
/// not an instance std made.
#[test]
fn an_extend_that_implements_an_interface_cant_store_its_parameter() {
    let e = err_src(&format!(
        "{PRELUDE}interface Mapper {{
           mapped<U>(f: (n: number) => U): U[];
         }}
         extend<T> Array<T> implements Mapper {{
           mapped<U>(f: (n: number) => U): U[] {{
             const out: U[] = [];
             for (let i = 0; i < this.length; i++) {{
               const s: G<T> = {{ v: this[i], n: i }};
               out.push(f(s.n));
             }}
             return out;
           }}
         }}
         function main() {{
           const a: string[] = [\"x\"];
           const m: Mapper = a;
           console.log(m.mapped((n: number) => n + 1));
         }}"
    ));
    assert!(e.contains("stores `T` in a struct's field"), "{e}");
    assert!(e.contains("take the interface off the `extend`"), "{e}");
    assert_eq!(e.matches("error:").count(), 1, "{e}");
}

/// The workarounds that message gives: its calls show the types an `extend` with no interface
/// runs at, and a field that doesn't depend on the `extend` holds no tuple.
#[test]
fn an_extend_is_legal_without_the_interface_or_with_a_type_of_its_own() {
    ok("extend<T> Array<T> {
          e(): number {
            const g: G<T> = { v: this[0], n: 7 };
            return g.n;
          }
        }
        interface Conc {
          c(): number;
        }
        extend<T> Array<T> implements Conc {
          c(): number {
            const g: G<number> = { v: 1, n: 7 };
            return g.n + this.length;
          }
        }
        function main() {
          const ns: number[] = [1];
          const ps: P3[] = [[1, 2, 3]];
          const h: Conc = ps;
          console.log(ns.e(), h.c());
        }");
}

/// A default method's parameter is the interface's, which the `extend` gives.
#[test]
fn a_default_method_counts_for_the_argument_its_extend_gives() {
    ok("interface Ho<X> {
          f(x: X): X {
            const g: G<X> = { v: x, n: 1 };
            return g.v;
          }
        }
        extend string implements Ho<number> {}
        function main() {
          const h: Ho<number> = \"a\";
          console.log(h.f(1));
        }");
}

/// What a method's class dispatches to is the class's, not the method's: `show` stores nothing.
#[test]
fn an_extend_counts_for_what_its_own_method_stores() {
    ok("function wrap<T>(x: T): number {
          const g: G<T> = { v: x, n: 1 };
          return g.n;
        }
        interface Wr {
          w(): number;
        }
        class Cell<T> implements Wr {
          v: T;
          constructor(v: T) {
            this.v = v;
          }
          w(): number {
            return wrap(this.v);
          }
        }
        interface Show {
          show(): string;
        }
        extend<T> Cell<T> implements Show {
          show(): string {
            return \"cell\";
          }
        }
        function main() {
          const s: Show = new Cell<number>(1);
          console.log(s.show());
        }");
}

/// The implementor here is an array, which is no tuple whatever its elements are.
#[test]
fn a_default_method_storing_what_cant_be_a_tuple_is_legal() {
    ok("function wrap<T>(x: T): number {
          const g: G<T> = { v: x, n: 1 };
          return g.n;
        }
        interface Me {
          me(): number {
            return wrap(this);
          }
        }
        extend<T> Array<T> implements Me {}
        function main() {
          const ns: number[] = [1];
          const m: Me = ns;
          console.log(m.me());
        }");
}

/// The error is at the interface's method, so it names the interface's parameter, and points at
/// the `extend`.
#[test]
fn a_default_method_is_reported_with_its_own_parameter() {
    let e = err_src(&format!(
        "{PRELUDE}interface D<X> {{
           d(x: X): number {{
             const g: G<X> = {{ v: x, n: 1 }};
             return g.n;
           }}
         }}
         extend<A> Array<A> implements D<A> {{}}
         function main() {{
           const ns: number[] = [1];
           const d: D<number> = ns;
           console.log(d.d(1));
         }}"
    ));
    assert!(
        e.contains("this default method stores `X` in a struct's field"),
        "{e}"
    );
    assert!(
        e.contains("this `extend` gives the interface the default method"),
        "{e}"
    );
}

/// Literal types aren't struct instances: they don't count toward the walk's limit.
#[test]
fn a_function_with_many_types_stays_legal() {
    let lits: String = (0..5000)
        .map(|i| format!("  const l{i}: {i} = {i};\n"))
        .collect();
    ok(&format!(
        "function f<T>(x: T): number {{
         {lits}
           const g = {{ v: x, n: 1 }} as G<T>;
           return g.n;
         }}
         function main() {{
           console.log(f<number>(1));
         }}"
    ));
}

/// The walk stops at a limit, so past it the check refuses: it can't tell.
#[test]
fn past_the_walks_limits_it_refuses() {
    let mut deep = String::from("struct S0<T> {\n  v: T | null;\n}\n");
    for i in 1..20 {
        let p = i - 1;
        deep += &format!("struct S{i}<T> {{\n  a: S{p}<T> | null;\n}}\n");
    }
    let e = err_src(&format!(
        "{deep}function f<T>(): number {{
           const s: S19<T> | null = null;
           return s == null ? 1 : 0;
         }}
         function main() {{
           console.log(f<number>());
         }}"
    ));
    assert!(e.contains("nested too deeply"), "{e}");

    let many: String = (0..4200)
        .map(|i| format!("  const l{i}: G<{i}> | null = null;\n"))
        .collect();
    let e = err_src(&format!(
        "{PRELUDE}function f<T>(x: T): number {{
         {many}
           const g: G<T> = {{ v: x, n: 1 }};
           return g.n;
         }}
         function main() {{
           console.log(f<number>(1));
         }}"
    ));
    assert!(e.contains("nested too deeply"), "{e}");
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

/// A write through a type parameter would change a struct, and the caller's value with it. A
/// bound alone is fine: the struct has the field, and reading it changes nothing.
#[test]
fn a_struct_cant_be_given_to_a_function_that_writes_its_field() {
    let set = "struct P {
                 x: number;
                 y: number;
               }
               function set<T extends { x: number }>(v: T): number {
                 v.x = 9;
                 return v.x;
               }
               function via<U extends { x: number }>(v: U): number {
                 return set(v);
               }
               class C {
                 x = 1;
               }";
    let e = err_src(&format!(
        "{set}
         function main() {{
           const p: P = {{ x: 1, y: 2 }};
           console.log(set(p), via(p));
         }}"
    ));
    assert!(e.contains("writes a field of a struct (`P`)"), "{e}");
    assert_eq!(e.matches("error:").count(), 2, "{e}");
    ok_src(&format!(
        "{set}
         function read<T extends {{ x: number }}>(v: T): number {{
           return v.x;
         }}
         function main() {{
           const p: P = {{ x: 1, y: 2 }};
           console.log(read(p), set(new C()), via(new C()));
         }}"
    ));
}

/// `as Record` makes a record of a copy of the fields, not a view: a struct is fine.
#[test]
fn a_struct_can_be_cast_to_a_record() {
    ok("struct P {
          x: number;
          y: number;
        }
        function main() {
          const p: P = { x: 1, y: 2 };
          const r = p as Record<string, number>;
          console.log(r[\"x\"]);
        }");
}

/// A `Proxy` of a type parameter is fine for what has identity: the check is at the struct.
#[test]
fn a_proxy_of_a_type_parameter_is_legal_for_a_class() {
    ok("class C {
          x = 1;
        }
        function logged<T extends object>(x: T): T {
          return new Proxy<T>(x, {});
        }
        function main() {
          const c = new C();
          console.log(logged(c).x, new Proxy<C>(c, {}).x, new Proxy(c, {}).x);
        }");
}

/// The usual way to write a `Proxy`: it would hand the caller a proxy where a struct is expected.
#[test]
fn a_struct_cant_be_given_to_a_function_that_makes_a_proxy_of_it() {
    let e = err_src(
        "struct P {
           x: number;
           y: number;
         }
         function logged<T extends object>(x: T): T {
           return new Proxy<T>(x, {});
         }
         function viaChain<U extends object>(x: U): U {
           return logged(x);
         }
         function take(p: P): number {
           return p.x;
         }
         function main() {
           const p: P = { x: 1, y: 2 };
           console.log(take(logged(p)), take(viaChain(p)));
         }",
    );
    assert!(e.contains("makes a `Proxy` of a struct (`P`)"), "{e}");
    assert_eq!(e.matches("error:").count(), 2, "{e}");
}

/// A class a method of such an `extend` makes is the method's own: what the class's methods
/// store runs at whatever reaches the `extend`. Only the class the `extend` is on is followed
/// wherever it is (`an_extend_counts_for_what_its_own_method_stores`).
#[test]
fn a_class_an_extend_makes_counts_as_its_own() {
    let e = err_src(&format!(
        "{PRELUDE}interface I<X> {{
           f(x: X): X;
         }}
         interface Ex<X> {{
           e(): X;
         }}
         class Impl<T> implements I<T> {{
           f(x: T): T {{
             const g: G<T> = {{ v: x, n: 7 }};
             return g.v;
           }}
         }}
         extend<T> Array<T> implements Ex<T> {{
           e(): T {{
             const i: I<T> = new Impl<T>();
             return i.f(this[0]);
           }}
         }}
         function main() {{
           const p: P3 = [1, 2, 3];
           const h: Ex<P3> = [p];
           console.log(h.e()[0]);
         }}"
    ));
    assert!(e.contains("this method of an `extend` stores `T`"), "{e}");
    assert_eq!(e.matches("error:").count(), 1, "{e}");
}

/// A value converted to `any` reaches every method of its class by name, while the program
/// reads members of an `any`: directly, through a type parameter, through an interface, and
/// through a field.
#[test]
fn a_value_converted_to_any_reaches_every_method() {
    let class = "class H<T> {
                   make(x: T): number {
                     const g: G<T> = { v: x, n: 7 };
                     return g.n;
                   }
                 }
                 interface Named {
                   name(): string;
                 }
                 class K<T> implements Named {
                   name(): string {
                     return \"k\";
                   }
                   make(x: T): number {
                     const g: G<T> = { v: x, n: 7 };
                     return g.n;
                   }
                 }
                 function wrap<T>(x: T): any {
                   return x;
                 }
                 class W {
                   h: H<P3> = new H<P3>();
                 }
                 class Box {
                   n: Named;
                   constructor(n: Named) {
                     this.n = n;
                   }
                 }";
    let main = |body: &str| {
        format!("{PRELUDE}{class}\nfunction main() {{\n  const t: P3 = [1, 2, 3];\n  {body}\n}}")
    };
    for body in [
        "const h: any = new H<P3>();\n  console.log(h.make(t));",
        "const h = wrap<H<P3>>(new H<P3>());\n  console.log(h.make(t));",
        "const n: Named = new K<P3>();\n  const h: any = n;\n  console.log(h.make(t));",
        // A member read through it gives its fields as dynamic values.
        "const a: any = new W();\n  console.log(a.h.make(t));",
        "const o: any = { h: new H<P3>() };\n  console.log(o.h.make(t));",
        "const b: any = new Box(new K<P3>());\n  console.log(b.n.make(t));",
    ] {
        let e = err_src(&main(body));
        assert!(
            e.contains("a method of this type stores a tuple"),
            "{body}: {e}"
        );
    }
    // A value that never becomes a dynamic one, or a program that reads no member of one.
    ok_src(&main("console.log(new H<P3>() == null);"));
    ok_src(&main(
        "const h: any = new H<P3>();\n  console.log(h == null);",
    ));
}

/// Glue runs a class's hooks for every value of it: the drop hook and the formatters'.
#[test]
fn a_hook_counts_wherever_its_class_is() {
    for (hook, body, use_) in [
        ("[Symbol.dispose](): void", "", "using h = new H<P3>(t);"),
        (
            "toString(): string",
            "return \"h\";",
            "console.log(String(new H<P3>(t)));",
        ),
    ] {
        let e = err_src(&format!(
            "{PRELUDE}class H<T> {{
               v: T;
               constructor(v: T) {{
                 this.v = v;
               }}
               {hook} {{
                 const g: G<T> = {{ v: this.v, n: 7 }};
                 console.log(g.n);
                 {body}
               }}
             }}
             function main() {{
               const t: P3 = [1, 2, 3];
               {use_}
             }}"
        ));
        assert!(e.contains("stores a tuple"), "{hook}: {e}");
    }
}

/// A closure made in a guarded branch keeps the guard: `f<P3>` drops the branch. Given what
/// may be a number, the branch stays.
#[test]
fn a_closure_keeps_the_guard_of_its_branch() {
    let src = |call: &str| {
        format!(
            "{PRELUDE}function isNum(x: unknown): x is number {{
               return typeof x == \"number\";
             }}
             function st<T>(x: T): number {{
               const g: G<T> = {{ v: x, n: 1 }};
               return g.n;
             }}
             function f<T>(x: T): number {{
               if (isNum(x)) {{
                 const c = () => st<T>(x);
                 return c();
               }}
               return 0;
             }}
             function main() {{
               const p: P3 = [1, 2, 3];
               console.log({call});
             }}"
        )
    };
    ok_src(&src("f<P3>(p)"));
    let e = err_src(&src("f<number | P3>(p)"));
    assert!(e.contains("this call stores a tuple"), "{e}");
}

/// A tuple given under a guard on a type parameter is for the callers to tell: `h<C>`, with a
/// class, drops the branch, and `h<number>` keeps it, so the error is at that call.
#[test]
fn a_tuple_given_under_a_guard_is_reported_where_the_branch_stays() {
    let src = |call: &str| {
        format!(
            "{PRELUDE}function isNum(x: unknown): x is number {{
               return typeof x == \"number\";
             }}
             function f<T, U>(x: T, u: U): number {{
               const g: G<U> = {{ v: u, n: 1 }};
               return g.n;
             }}
             class C {{
               a = 1;
             }}
             function h<T>(x: T): number {{
               if (isNum(x)) {{
                 const p: P3 = [1, 2, 3];
                 return f<T, P3>(x, p);
               }}
               return 0;
             }}
             function main() {{
               console.log({call});
             }}"
        )
    };
    ok_src(&src("h<C>(new C())"));
    let bad = src("h<C>(new C()), h<number>(1)");
    let e = err_src(&bad);
    assert!(e.contains("this call stores a tuple"), "{e}");
    let at = bad.find("h<number>").unwrap();
    let line = bad[..at].matches('\n').count() + 1;
    let col = at - bad[..at].rfind('\n').unwrap();
    assert!(e.contains(&format!(":{line}:{col}: error")), "{e}");
    assert_eq!(e.matches("error:").count(), 1, "{e}");
}

/// The type an `extend` is on is followed wherever it is, but another instance of its class
/// isn't: `Cell<T>` made in `extend<T> Cell<T[]>` is the method's own.
#[test]
fn another_instance_of_the_class_an_extend_is_on_counts_as_its_own() {
    let e = err_src(&format!(
        "{PRELUDE}interface Ex<X> {{
           e(): X;
         }}
         class Base<T> {{
           v: T;
           constructor(v: T) {{
             this.v = v;
           }}
           f(): T {{
             return this.v;
           }}
         }}
         class Cell<T> extends Base<T> {{
           override f(): T {{
             const g: G<T> = {{ v: this.v, n: 7 }};
             return g.v;
           }}
         }}
         extend<T> Cell<T[]> implements Ex<T> {{
           e(): T {{
             const b: Base<T> = new Cell<T>(this.v[0]);
             return b.f();
           }}
         }}
         function main() {{
           const p: P3 = [1, 2, 3];
           const h: Ex<P3> = new Cell<P3[]>([p]);
           console.log(h.e()[0]);
         }}"
    ));
    assert!(e.contains("this method of an `extend` stores `T`"), "{e}");
}
