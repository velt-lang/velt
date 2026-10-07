//! Shared helpers for the parser tests: parsing shortcuts, a compact S-expression printer for
//! expressions/types/patterns, golden-file discovery and a kitchen-sink source covering the whole
//! AST surface.
// Each test binary compiles this module separately and uses a different subset of it.
#![allow(dead_code, unused_imports)]

use std::path::{Path, PathBuf};

pub use velt_common::{Diagnostics, FileId, SourceMap, Span};
pub use velt_syntax::ast::*;
pub use velt_syntax::{dump, parse_file};

pub fn parse(src: &str) -> (Module, Diagnostics) {
    parse_file(FileId(0), src)
}

pub fn parse_ok(src: &str) -> Module {
    let (m, d) = parse(src);
    assert!(
        d.is_empty(),
        "unexpected diagnostics for:\n{}\n{:#?}",
        src,
        d.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    m
}

pub fn errors(src: &str) -> Vec<String> {
    parse(src).1.into_iter().map(|d| d.message).collect()
}

/// Body statements of the first function in the module.
pub fn body(m: &Module) -> &[Stmt] {
    match &m.items[0].kind {
        ItemKind::Function(f) => &f.body.stmts,
        k => panic!("expected function, got {:?}", k),
    }
}

/// Parses `src` as the single expression statement of a function body.
pub fn expr(src: &str) -> Expr {
    let m = parse_ok(&format!("function f() {{ {}; }}", src));
    match &body(&m)[0].kind {
        StmtKind::Expr(e) => e.clone(),
        k => panic!("expected expression statement, got {:?}", k),
    }
}

pub fn lit(l: &Lit) -> String {
    match l {
        Lit::Int { value, suffix } => format!("{}{}", value, suffix.as_deref().unwrap_or("")),
        Lit::Float { value, suffix } => format!("{:?}{}", value, suffix.as_deref().unwrap_or("")),
        Lit::Str(s) => format!("{:?}", s),
        Lit::Bool(b) => b.to_string(),
        Lit::Null => "null".into(),
    }
}

pub fn ty(t: &TypeExpr) -> String {
    match &t.kind {
        TypeExprKind::Named { path, args } => {
            let p = path
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if args.is_empty() {
                p
            } else {
                format!(
                    "{}<{}>",
                    p,
                    args.iter().map(ty).collect::<Vec<_>>().join(", ")
                )
            }
        }
        TypeExprKind::Array(t) => format!("{}[]", ty(t)),
        TypeExprKind::Tuple(ts) => {
            format!("[{}]", ts.iter().map(ty).collect::<Vec<_>>().join(", "))
        }
        TypeExprKind::Function {
            params,
            ret,
            throws,
        } => {
            let throws = throws
                .as_ref()
                .map(|t| format!(" throws {}", ty(t)))
                .unwrap_or_default();
            format!(
                "fn({}) => {}{throws}",
                params.iter().map(ty).collect::<Vec<_>>().join(", "),
                ty(ret)
            )
        }
        TypeExprKind::Union(ts) => {
            format!("({})", ts.iter().map(ty).collect::<Vec<_>>().join(" | "))
        }
        TypeExprKind::Literal(l) => pat_lit(l),
        TypeExprKind::Object(fs) => {
            let parts: Vec<String> = fs
                .iter()
                .map(|f| format!("{}: {}", f.name.name, ty(&f.ty)))
                .collect();
            format!("{{{}}}", parts.join("; "))
        }
        TypeExprKind::Null => "null".into(),
        TypeExprKind::Void => "void".into(),
    }
}

pub fn binop(op: BinaryOp) -> &'static str {
    use BinaryOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Rem => "%",
        Pow => "**",
        Eq => "==",
        NotEq => "!=",
        Lt => "<",
        LtEq => "<=",
        Gt => ">",
        GtEq => ">=",
        And => "&&",
        Or => "||",
        Nullish => "??",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        Shl => "<<",
        Shr => ">>",
        UShr => ">>>",
        In => "in",
    }
}

pub fn props(ps: &[ObjectProp]) -> String {
    ps.iter()
        .map(|p| match p {
            ObjectProp::KeyValue(k, v) => format!("{}: {}", k.name, sx(v)),
            ObjectProp::Shorthand(k) => k.name.clone(),
            ObjectProp::Spread(e) => format!("...{}", sx(e)),
            ObjectProp::Method(f) => format!("{}()", f.sig.name.name),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn pat(p: &Pattern) -> String {
    match &p.kind {
        PatternKind::Ident(i) => i.name.clone(),
        PatternKind::Wildcard => "_".into(),
        PatternKind::Object { fields, rest } => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|(k, p)| format!("{}: {}", k.name, pat(p)))
                .collect();
            if let Some(r) = rest {
                parts.push(format!("...{}", r.name));
            }
            format!("{{{}}}", parts.join(", "))
        }
        PatternKind::Array { elems, rest } => {
            let mut parts: Vec<String> = elems.iter().map(pat).collect();
            if let Some(r) = rest {
                parts.push(format!("...{}", r.name));
            }
            format!("[{}]", parts.join(", "))
        }
        PatternKind::Default { pattern, value } => format!("{} = {}", pat(pattern), sx(value)),
    }
}

/// Compact S-expression rendering of an expression, for precedence tests.
pub fn sx(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Lit(l) => lit(l),
        ExprKind::Template { quasis, exprs } => {
            let mut s = String::from("`");
            for (i, q) in quasis.iter().enumerate() {
                s.push_str(q);
                if let Some(e) = exprs.get(i) {
                    s.push_str(&format!("${{{}}}", sx(e)));
                }
            }
            s.push('`');
            s
        }
        ExprKind::Ident(i) => i.name.clone(),
        ExprKind::This => "this".into(),
        ExprKind::Super => "super".into(),
        ExprKind::Unary { op, expr } => {
            let o = match op {
                UnaryOp::Neg => "-",
                UnaryOp::Plus => "+",
                UnaryOp::Not => "!",
                UnaryOp::BitNot => "~",
                UnaryOp::TypeOf => "typeof",
                UnaryOp::Delete => "delete",
            };
            format!("({} {})", o, sx(expr))
        }
        ExprKind::Binary { op, lhs, rhs } => format!("({} {} {})", binop(*op), sx(lhs), sx(rhs)),
        ExprKind::Assign { op, target, value } => {
            format!("({}= {} {})", op.map_or("", binop), sx(target), sx(value))
        }
        ExprKind::Update { op, prefix, target } => {
            let o = if *op == UpdateOp::Inc { "++" } else { "--" };
            if *prefix {
                format!("({}pre {})", o, sx(target))
            } else {
                format!("({}post {})", o, sx(target))
            }
        }
        ExprKind::Cond { cond, then, els } => format!("(? {} {} {})", sx(cond), sx(then), sx(els)),
        ExprKind::Call {
            callee,
            type_args,
            args,
            ..
        } => {
            let ta = if type_args.is_empty() {
                String::new()
            } else {
                format!(
                    "<{}>",
                    type_args.iter().map(ty).collect::<Vec<_>>().join(", ")
                )
            };
            let a = args.iter().map(sx).collect::<Vec<_>>();
            format!("(call {}{} [{}])", sx(callee), ta, a.join(" "))
        }
        ExprKind::New { class, args } => {
            format!(
                "(new {} [{}])",
                ty(class),
                args.iter().map(sx).collect::<Vec<_>>().join(" ")
            )
        }
        ExprKind::Member {
            object,
            prop,
            optional,
        } => {
            format!(
                "({} {} {})",
                if *optional { "?." } else { "." },
                sx(object),
                prop.name
            )
        }
        ExprKind::Index { object, index, .. } => format!("([] {} {})", sx(object), sx(index)),
        ExprKind::Function(f) => format!(
            "({}function{} {}({}) {{{} stmts}})",
            if f.sig.is_async { "async " } else { "" },
            if f.sig.is_generator { "*" } else { "" },
            f.sig.name.name,
            f.sig.params.len(),
            f.body.stmts.len()
        ),
        ExprKind::Arrow {
            type_params,
            params,
            ret,
            throws,
            body,
            is_async,
        } => {
            let tps = type_params
                .iter()
                .map(|g| match g.bounds.as_slice() {
                    [] => g.name.name.clone(),
                    bs => format!(
                        "{} extends {}",
                        g.name.name,
                        bs.iter().map(ty).collect::<Vec<_>>().join(" & ")
                    ),
                })
                .collect::<Vec<_>>();
            let tps = if tps.is_empty() {
                String::new()
            } else {
                format!("<{}>", tps.join(", "))
            };
            let ps = params
                .iter()
                .map(|p| match &p.ty {
                    Some(t) => format!("{}: {}", p.name.name, ty(t)),
                    None => p.name.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            let r = ret
                .as_ref()
                .map(|t| format!(": {}", ty(t)))
                .unwrap_or_default()
                + &throws
                    .as_ref()
                    .map(|t| format!(" throws {}", ty(t)))
                    .unwrap_or_default();
            let b = match body {
                ArrowBody::Expr(e) => sx(e),
                ArrowBody::Block(b) => format!("{{{} stmts}}", b.stmts.len()),
            };
            format!(
                "({}arrow {}({}){} {})",
                if *is_async { "async " } else { "" },
                tps,
                ps,
                r,
                b
            )
        }
        ExprKind::Array(es) => format!("[{}]", es.iter().map(sx).collect::<Vec<_>>().join(", ")),
        ExprKind::Object(ps) => format!("{{{}}}", props(ps)),
        ExprKind::StructLit { name, props: ps } => format!("{} {{{}}}", ty(name), props(ps)),
        ExprKind::Spread(e) => format!("...{}", sx(e)),
        ExprKind::Await(e) => format!("(await {})", sx(e)),
        ExprKind::Yield { arg, delegate } => {
            let kw = if *delegate { "yield*" } else { "yield" };
            match arg {
                Some(a) => format!("({kw} {})", sx(a)),
                None => format!("({kw})"),
            }
        }
        ExprKind::Cast { expr, ty: t } => format!("(as {} {})", sx(expr), ty(t)),
        ExprKind::InstanceOf { expr, ty: t } => format!("(instanceof {} {})", sx(expr), ty(t)),
        ExprKind::Paren(e) => format!("(paren {})", sx(e)),
        ExprKind::NonNull(e) => format!("(! {})", sx(e)),
        ExprKind::Jsx(el) => jsx(el),
    }
}

/// Compact rendering of a JSX element: `<div a="v" b={x} {...p}>["text" {y} {} <br/>]`
/// (fragments have an empty name; no children = `/>`; type arguments as `<List<i64>`).
pub fn jsx(el: &JsxElement) -> String {
    let mut name = el.name.as_ref().map(JsxName::to_source).unwrap_or_default();
    if !el.type_args.is_empty() {
        let args: Vec<String> = el.type_args.iter().map(ty).collect();
        name = format!("{name}<{}>", args.join(", "));
    }
    let attrs: String = el
        .attrs
        .iter()
        .map(|a| match a {
            JsxAttr::Spread { expr, .. } => format!(" {{...{}}}", sx(expr)),
            JsxAttr::Named { name, value, .. } => {
                let v = match value {
                    None => String::new(),
                    Some(JsxAttrValue::Str { value, .. }) => format!("={value:?}"),
                    Some(JsxAttrValue::Expr { expr, .. }) => format!("={{{}}}", sx(expr)),
                    Some(JsxAttrValue::Element(inner)) => format!("={}", jsx(inner)),
                };
                format!(" {}{v}", name.to_source())
            }
        })
        .collect();
    if el.children.is_empty() {
        return format!("<{name}{attrs}/>");
    }
    let kids: Vec<String> = el
        .children
        .iter()
        .map(|c| match c {
            JsxChild::Text { value, .. } => format!("{value:?}"),
            JsxChild::Expr { expr: None, .. } => "{}".into(),
            JsxChild::Expr { expr: Some(e), .. } => format!("{{{}}}", sx(e)),
            JsxChild::Spread { expr, .. } => format!("{{...{}}}", sx(expr)),
            JsxChild::Element(inner) => jsx(inner),
        })
        .collect();
    format!("<{name}{attrs}>[{}]", kids.join(" "))
}

pub fn check(src: &str, expected: &str) {
    assert_eq!(sx(&expr(src)), expected, "source: {}", src);
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn golden_files() -> Vec<PathBuf> {
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|e| e == "vlt") {
                out.push(p);
            }
        }
    }
    let mut out = vec![];
    collect(&workspace_root().join("tests/golden"), &mut out);
    out
}

/// One function of ordinary code (six lines): arithmetic, control flow, templates, a generic call.
pub const PLAIN_UNIT: &str = "function f(a: i64, b: i64): i64 {\n  let x = a * 2 + b / 3 - (a % 7);\n  if (x > 10 && b < 3) { return x; } else { x += 1; }\n  for (let i = 0; i < 10; i++) { console.log(`i=${i} x=${x}`, \"s\\n\"); }\n  return g<i64>(x, [1, 2, 3], { a: 1, b }) as i64;\n}\n";

/// Three items whose JSX elements and generic arrows the parser, not the lexer, decides: every
/// element is re-lexed once where the parser finds it.
pub const JSX_UNIT: &str =
    "const a = <ul class=\"x\">{xs.map((i) => <li key={i}>it's {i}</li>)}</ul>;\n\
                            const id = <T>(x: T): T => x;\nconst u = v.as<User>();\n";

/// Source exercising every construct of the AST surface; must parse without diagnostics.
pub const KITCHEN_SINK: &str = r#"
import { readFile, writeFile as wf } from "velt:fs";
import {} from "./side";
export function add<T extends Num & Copy, U>(a: T, b: U = 1, f: (x: i64) => i64): T | null { return a; }
export async function fetchAll(urls: string[]): Promise<string[]> { const r = await Promise.all(urls); return r; }
struct Point implements Show, Eq { x: f64, y: f64; readonly z: i32 = 0; len(): f64 { return this.x; } scale(k: f64) { this.x *= k; } static origin(): Point { return Point { x: 0.0, y: 0.0, z: 0 }; } }
class User { name: string; constructor(name: string) { this.name = name; } async greet(): string { return `hi ${this.name}`; } }
interface Shape<T> extends Base { area(): f64; name(): string, async load(p: T): void }
type Shape = { kind: "circle"; r: f64 } | { kind: "rect"; w: f64; h: f64 };
enum Color { Red, Green = 5, Blue, }
enum Dir { Up = "UP", Down = "DOWN" }
type Id = u64;
type Handler<T> = (req: T, res: [i32, string]) => void;
export const MAX: i64 = 1_000_000;
let counter = 0;
declare function velt_rt_print(s: string, n: i64): void;
function main(): i32 {
  const { a, b: [c, d], ...rest } = obj;
  let [x, , y] = pair;
  let [first, ...others] = xs;
  const f = (a: i64, b) => a + b;
  const g = async (s: string): string => { return s; };
  const h = x => x * 2;
  const k = async y => await y;
  const e = () => {};
  const o = { a: 1, b, ...c, "quoted": 2, type: 3 };
  const arr = [1, ...xs, 3,];
  const n = new Map<string, i64>();
  const p = a?.b?.c ?? d;
  const q = load();
  const fe: (x: i64) => i64 throws E = (x: i64): i64 throws E => x;
  const r = x as f64 / 2.0;
  switch (shape.kind) {
    case "circle":
    case "rect": { s = 1; break; }
    case 1 + 2: s = 2;
    default:
      s = 0;
  }
  const t = `a ${`nested ${x + `deep ${y}`}`} b ${ {k: 1}.k }`;
  const u = f<i64>(1) + g<Map<K, Array<V>>>(2);
  const v = a < b && c > d;
  const w = 0xff + 0b1010 + 0o17 + 1e21 + 2.5e-3 + 10u8 + 1.0f32 + .5;
  x >>>= 2; x >>= 1; x **= 2; x ??= 3; x &&= y; x ||= z;
  if (a) b(); else if (c) d(); else { e(); }
  while (i < 10) i++;
  do { k--; } while (k > 0);
  for (let i = 0; i < 10; i++) {}
  for (;;) break;
  for (const [k, v] of map) { continue; }
  outer: for (i = 0; i < 3; ++i) { break outer; }
  try { risky(); } catch (e) { throw e; } finally { cleanup(); }
  try { risky(); } catch { }
  function inner(): void {}
  struct Local { v: i32 }
  type L = i32;
  ;
  const id = <T,>(x: T): T => x;
  const id2 = <T>(x: T): T => x;
  const u = v.as<User>() ?? v?.as<User>();
  const page = (
    <>
      <ui.Card title="a &amp; b" data-id={id(1)} {...rest} svg:x="1" disabled>
        Hello, {name}! {/* note */}
        {...items}
        <br />
      </ui.Card>
    </>
  );
  return c ? 1 : 2;
}
"#;

fn pat_lit(l: &SignedLit) -> String {
    format!("{}{}", if l.negative { "-" } else { "" }, lit(&l.lit))
}
