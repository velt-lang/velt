//! Generic interface values converted to the same interface at other type arguments through an
//! adapter (`body/expr/iface_adapt.rs`, #1185).

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn a_slot_of_a_mapped_type_over_any_converts() {
    // `Acc<any>`, a homomorphic mapped type over `any`, is `any` (it was an unresolved slot
    // while `keyof any` was not supported): the value converts to `Ctx<any>`.
    ok_src(
        "type Acc<T> = { readonly [K in keyof T]: T[K] };
         interface Ctx<P = {}> { props: Acc<P>; name(): string; }
         function take(c: Ctx<any>): string { return c.name(); }
         function main() {
           const c: Ctx<{}> = { props: {}, name(): string { return \"c\"; } };
           console.log(take(c));
         }",
    );
}

#[test]
fn fields_are_written_through_an_adapter() {
    ok_src(
        "interface Ctx<P = {}> { props: P; label: string; name(): string; }
         function rename(c: Ctx<any>): void { c.label = \"b\"; c.props = 3; }
         function main() {
           const c: Ctx<number> = { props: 1, label: \"a\", name(): string { return \"c\"; } };
           rename(c);
           console.log(c.label, c.props);
         }",
    );
}

#[test]
fn a_field_that_does_not_convert_back_is_reported_where_it_is_written() {
    let r = err_src(
        "interface Holder<T> { item: T; get(): T; }
         interface Box<T> { inner: Holder<T>; size(): number; }
         function put(b: Box<unknown>): void { b.inner = b.inner; }
         function main() {
           const h: Holder<number> = { item: 1, get(): number { return 1; } };
           const b: Box<number> = { inner: h, size(): number { return 1; } };
           put(b);
         }",
    );
    assert!(r.contains("would not convert back"), "{r}");
}

#[test]
fn a_conversion_out_of_unknown_gets_one_generic_adapter() {
    // Written before the implementations exist: made once every body is checked, over the
    // arguments it adapts from, so it serves every instance (lowering's too).
    let p = ok_src(
        "interface Ctx<P = {}> { props: P; name(): string; }
         function read(u: unknown): Ctx<any> { return u as Ctx<any>; }
         class G<T> implements Ctx<T> {
           props: T;
           constructor(p: T) { this.props = p; }
           name(): string { return \"g\"; }
         }
         function main() {
           const c: Ctx<{ n: number }> = { props: { n: 1 }, name(): string { return \"c\"; } };
           console.log(read(c).name(), read(new G<string>(\"s\")).name());
         }",
    );
    let wrapped: Vec<(u32, String)> = (p.impls.iter())
        .filter_map(|i| match p.def(adt(&p, i.ty)?) {
            velt_sema::hir::Def::Adt(a) if a.name.ends_with(" adapter") => {
                let velt_sema::hir::TyKind::Dyn(_, args) = p.types.kind(a.fields[0].ty) else {
                    return None;
                };
                Some((i.generics, format!("{:?}", p.types.kind(args[0]))))
            }
            _ => None,
        })
        .collect();
    assert_eq!(wrapped, [(1, "Param(0)".to_string())]);
}

#[test]
fn an_unwritable_adapter_field_is_reported_only_when_written() {
    let src = |write: &str| {
        format!(
            "interface Tree<T> {{ value: T; kids: Tree<T>[]; label(): string; }}
             class N<T> implements Tree<T> {{
               value: T; kids: Tree<T>[] = [];
               constructor(v: T) {{ this.value = v; }}
               label(): string {{ return \"n\"; }}
             }}
             function main() {{
               const t: Tree<number> = new N<number>(1);
               const a: Tree<any> = t;
               t.value = 2;
               {write}
               console.log(a.kids.length);
             }}"
        )
    };
    ok_src(&src(""));
    let r = err_src(&src("a.kids = [];"));
    assert!(r.contains("writing `kids` through `Tree<any>`"), "{r}");
    // A change through the array read is an error of its own, without a field write.
    let r = err_src(&src("a.kids.push(a);"));
    assert!(
        r.contains("`kids` read through `Tree<any>` may be a new array"),
        "{r}"
    );
}

fn adt(p: &velt_sema::hir::Program, t: velt_sema::hir::TyId) -> Option<velt_sema::hir::DefId> {
    match p.types.kind(t) {
        velt_sema::hir::TyKind::Adt(d, _) => Some(*d),
        _ => None,
    }
}
