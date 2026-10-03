use velt_common::FileId;

use super::{walk_module, Visit};
use crate::ast;

/// Records what each callback saw, as source text.
#[derive(Default)]
struct Seen {
    src: String,
    items: usize,
    functions: Vec<String>,
    vars: usize,
    exprs: Vec<String>,
    types: Vec<String>,
}

impl<'a> Visit<'a> for Seen {
    fn item(&mut self, _item: &'a ast::Item) {
        self.items += 1;
    }
    fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
        self.functions.push(sig.name.name.clone());
    }
    fn var_decl(&mut self, _v: &'a ast::VarDecl) {
        self.vars += 1;
    }
    fn expr(&mut self, e: &'a ast::Expr) {
        self.exprs.push(self.text(e.span));
    }
    fn ty(&mut self, t: &'a ast::TypeExpr) {
        self.types.push(self.text(t.span));
    }
}

impl Seen {
    fn text(&self, span: velt_common::Span) -> String {
        self.src[span.lo as usize..span.hi as usize].to_string()
    }
}

fn walk(src: &str) -> Seen {
    let (module, diags) = crate::parse_file(FileId(0), src);
    assert!(diags.is_empty(), "{diags:?}");
    let mut seen = Seen {
        src: src.to_string(),
        ..Seen::default()
    };
    walk_module(&module, &mut seen);
    seen
}

#[test]
fn visits_items_functions_and_declarations_in_source_order() {
    let seen = walk(
        "class C { x: i32 = 1; m(): void { function inner() {} } }\n\
         const k = 2;\n\
         function main() { let y = k + 1; }\n",
    );
    assert_eq!(seen.items, 4, "class, nested function, const, main");
    assert_eq!(seen.functions, ["m", "inner", "main"]);
    assert_eq!(seen.vars, 2);
    assert_eq!(seen.exprs, ["1", "2", "k + 1", "k", "1"]);
}

#[test]
fn visits_every_written_type_nested_ones_included() {
    let seen = walk(
        "type A = Map<string, u8[]>;\n\
         interface I { f(x: i16): void; }\n\
         function g<T extends I>(p: T): i64 throws Error {\n\
           const f = (a: f32): f64 => a as f64;\n\
           return new Array<bool>(0).length;\n\
         }\n",
    );
    for want in [
        "Map<string, u8[]>",
        "string",
        "u8[]",
        "u8",
        "i16",
        "I",
        "T",
        "i64",
        "Error",
        "f32",
        "f64",
        "Array<bool>",
        "bool",
    ] {
        assert!(
            seen.types.iter().any(|t| t == want),
            "{want} in {:?}",
            seen.types
        );
    }
}

#[test]
fn visits_jsx_and_arrow_bodies() {
    let seen = walk("function v(n: i64) { return <p a={n}>{[1].map((x) => x + 1)}</p>; }\n");
    assert!(seen.exprs.iter().any(|e| e == "x + 1"), "{:?}", seen.exprs);
    assert!(seen.exprs.iter().any(|e| e == "n"));
}
