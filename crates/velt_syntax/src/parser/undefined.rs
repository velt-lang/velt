//! `undefined` (a value and a type, issue #930) and `void expr` (`UnaryOp::Void`) are parsed by
//! `primary`, `types` and `expr`; this module holds their tests.

#[cfg(test)]
mod tests {
    use crate::ast::{ExprKind, Lit, TypeExprKind, UnaryOp};
    use crate::parse_file;
    use velt_common::FileId;

    fn messages(src: &str) -> Vec<(String, Vec<String>)> {
        let (_, diags) = parse_file(FileId(0), src);
        diags.into_iter().map(|d| (d.message, d.notes)).collect()
    }

    #[test]
    fn undefined_value_type_and_void_parse() {
        let src =
            "function f(a: string | undefined) {\n  const x = undefined;\n  const y = void 0;\n}";
        assert!(messages(src).is_empty());
        let (file, _) = parse_file(
            FileId(0),
            "const x = undefined; void g(); let t: undefined;",
        );
        let shown = format!("{:?}", file.items);
        assert!(shown.contains(&format!("{:?}", ExprKind::Lit(Lit::Undefined))));
        assert!(shown.contains(&format!("op: {:?}", UnaryOp::Void)));
        assert!(shown.contains(&format!("{:?}", TypeExprKind::Undefined)));
    }

    #[test]
    fn undefined_as_a_protocol_types_return_type_is_left_to_sema() {
        let src = "function f(r: IteratorResult<number, undefined>, i: IterableIterator<string, undefined>, o: IteratorObject<i64, undefined>, a: AsyncIterableIterator<i64, undefined>) {}";
        assert!(messages(src).is_empty());
    }

    #[test]
    fn a_finished_result_drops_value_undefined() {
        let src = "function f() { return { done: true, value: undefined }; }";
        assert!(messages(src).is_empty());
        let (file, _) = parse_file(FileId(0), src);
        let shown = format!("{:?}", file.items);
        assert!(!shown.contains("\"value\""), "{shown}");
        // Elsewhere, `value: undefined` is a value.
        let src = "function f() { return { done: false, value: undefined }; }";
        assert!(messages(src).is_empty());
        let (file, _) = parse_file(FileId(0), src);
        assert!(format!("{:?}", file.items).contains("\"value\""));
    }

    #[test]
    fn yield_may_be_a_branch_of_a_conditional() {
        assert!(messages("function* g(c: bool) { c ? yield 1 : yield 2; }").is_empty());
    }

    #[test]
    fn undefined_as_a_property_name_is_fine() {
        assert!(messages("function f() { g(o.undefined); }").is_empty());
    }
}
