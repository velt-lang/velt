//! The size of code converting an `any` to an object type (#2189). Each conversion used to write
//! its whole slow path out at the site: every other way the value may convert, a copy member
//! by member with each member converted the same way, and the `TypeError`'s messages. One
//! `as JSXElement` (a union with a nine-member object type) came to about 1,550 blocks, so a
//! 350-line generator rendering JSX trees through `any` (sigx's `renderNode`) lowered to a
//! state machine of 40,000 blocks, and every pass after lowering and LLVM paid for them.
//!
//! A site now keeps the comparison with the target's descriptor and calls the target's
//! `AnyUnbox` glue for the rest. The checks: in the VIR of a generator and of a plain function
//! with N such sites, each site costs a few dozen blocks, the generator's suspensions add
//! little, and the program still converts and reports as before.

mod no_window;
mod test_dir;

const TYPES: &str = "type VNode = {
    type: string | Function;
    props: Record<string, any>;
    key: string | number | null;
    children: VNode[];
    dom: any | null;
    text?: string | number;
    parent?: VNode | null;
    cleanup?: () => void;
};
type JSXElement = VNode | string | number | boolean | null;
function sink(e: JSXElement, buf: string[]): boolean {
    if (typeof e === \"string\") { buf.push(e); return true; }
    if (typeof e === \"number\") { buf.push(`n${e}`); return true; }
    if (e !== null && typeof e === \"object\") { buf.push(`v${e.children.length}`); return true; }
    return false;
}
";

/// A program whose `walk` converts `n` `any` values to `JSXElement` inside `try`/`catch`/
/// `finally`; a generator yielding after each site, or a plain function.
fn program(n: usize, generator: bool) -> String {
    let mut s = String::from(TYPES);
    if generator {
        s.push_str(
            "function* walk(src: any[], buf: string[]): Generator<number, void, unknown> throws any {\n",
        );
    } else {
        s.push_str("function walk(src: any[], buf: string[]): void throws any {\n");
    }
    s.push_str("    try {\n");
    for i in 0..n {
        s.push_str(&format!(
            "        const v{i}: any = src[{i} % src.length];\n"
        ));
        s.push_str(&format!(
            "        if (!sink(v{i}, buf)) {{ buf.push(\"x{i}\"); }}\n"
        ));
        if generator {
            s.push_str(&format!("        yield {i};\n"));
        }
    }
    s.push_str("    } catch (e) { buf.push(\"caught\"); } finally { buf.push(\"f\"); }\n}\n");
    s.push_str(
        "const buf: string[] = [];
const node: any = { type: \"div\", props: {}, key: null, children: [], dom: null };
const src: any[] = [\"a\", 2, node, false, null];
",
    );
    if generator {
        s.push_str("for (const k of walk(src, buf)) { buf.push(String(k)); }\n");
    } else {
        s.push_str("walk(src, buf);\n");
    }
    s.push_str("console.log(buf.join(\" \"));\n");
    s
}

fn velt(src: &str, args: &[&str]) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .arg("main.vlt")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Blocks of the function whose symbol contains `name`, in `velt build --emit vir` output.
fn blocks_of(vir: &str, name: &str) -> usize {
    let start = vir
        .lines()
        .position(|l| l.starts_with("fn#") && l.contains(name))
        .unwrap_or_else(|| panic!("no `{name}` in the VIR"));
    vir.lines()
        .skip(start + 1)
        .take_while(|l| *l != "}")
        .filter(|l| l.starts_with("  bb") && l.ends_with(':'))
        .count()
}

#[test]
fn an_any_converted_to_an_object_type_costs_few_blocks_per_site() {
    let (few, many) = (4, 12);
    let size = |n: usize, generator: bool| {
        let name = if generator {
            "_V4walk$poll("
        } else {
            "_V4walk("
        };
        blocks_of(
            &velt(&program(n, generator), &["build", "--emit", "vir"]),
            name,
        )
    };
    for generator in [false, true] {
        let (a, b) = (size(few, generator), size(many, generator));
        let per_site = (b - a) / (many - few);
        // Written out at each site, a conversion was about 1,550 blocks.
        assert!(
            per_site <= 200,
            "{per_site} blocks per site (generator: {generator}): {a} blocks at {few} sites, {b} at {many}"
        );
    }
}

#[test]
fn conversions_through_the_glue_convert_and_report_as_before() {
    let expect = "a 0 n2 1 v0 2 x3 3 x4 4 a 5 f\n";
    assert_eq!(velt(&program(6, true), &["run"]), expect);
    assert_eq!(velt(&program(6, false), &["run"]), "a n2 v0 x3 x4 a f\n");
}
