//! Generic interface values converted to the same interface at other type arguments through an
//! adapter (`body/expr/iface_adapt.rs`, #1185).

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn a_slot_whose_type_is_unresolved_does_not_refuse_the_conversion() {
    // `keyof any` is reported where the mapped type is written; the conversion of the value
    // whose field has that type is not reported again at each use.
    let r = err_src(
        "type Acc<T> = { readonly [K in keyof T]: T[K] };
         interface Ctx<P = {}> { props: Acc<P>; name(): string; }
         function take(c: Ctx<any>): string { return c.name(); }
         function main() {
           const c: Ctx<{}> = { props: {}, name(): string { return \"c\"; } };
           console.log(take(c));
         }",
    );
    assert!(r.contains("`keyof any` is not supported yet"), "{r}");
    assert!(!r.contains("mismatched types"), "{r}");
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
