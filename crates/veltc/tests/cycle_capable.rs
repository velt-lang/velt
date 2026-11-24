//! Which counted types can be part of a reference cycle (docs/internals/design/cycles.md
//! section 1): those on a cycle of the "may own a strong reference to" graph, including the
//! edges through closures, interface values, class hierarchies, uniquely owned objects and
//! weak maps' entries. Counted types that cannot reach themselves stay out.

mod no_window;
mod test_dir;

/// What lowering prints for `src` under `VELT_DEBUG_COUNTED=1` (stderr).
fn debug_out(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
        .env("VELT_DEBUG_COUNTED", "1")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    stderr
}

/// The VIR lowering prints for `src` (`--emit vir`).
fn vir_out(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
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

/// The `velt: cycle-capable types:` line lowering prints for `src` (`VELT_DEBUG_COUNTED=1`).
/// Type names may contain commas (object types), so the line is kept whole.
fn cycle_capable(src: &str) -> String {
    let stderr = debug_out(src);
    stderr
        .lines()
        .find_map(|l| l.strip_prefix("velt: cycle-capable types:"))
        .unwrap_or_else(|| panic!("no cycle-capable types reported:\n{stderr}"))
        .trim()
        .to_string()
}

/// Is `name` one of the comma-separated entries of `line` (an exact entry, not a substring
/// of a longer name)?
fn has(line: &str, name: &str) -> bool {
    line.split(", ").any(|e| e == name)
}

/// Does an entry of `line` name closure environments (of any function type)?
fn has_closures(line: &str) -> bool {
    line.contains("closure environments of")
}

#[test]
fn a_listener_closure_capturing_its_view_closes_a_cycle() {
    let c = cycle_capable(
        "class Button {
           listeners: (() => void)[] = [];
           onClick(f: () => void) { this.listeners.push(f); }
           click() { for (const f of this.listeners) f(); }
         }
         class View { clicks = 0; button = new Button(); }
         function main() {
           const v = new View();
           const b = v.button;
           b.onClick(() => { v.clicks += 1; });
           b.click();
           console.log(v.clicks);
         }",
    );
    assert!(has(&c, "class View"), "{c}");
    assert!(has(&c, "class Button"), "{c}");
}

#[test]
fn a_const_used_by_its_own_closures_closes_a_cycle() {
    let c = cycle_capable(
        "interface App { name: string; use(p: (a: App) => void): App; }
         function makeApp(name: string): App {
           const app: App = { name, use(p) { p(app); return app; } };
           return app;
         }
         function main() {
           const a = makeApp(\"x\");
           a.use((x) => console.log(x.name));
         }",
    );
    // The literal's method environment holds the cell of `app`, which holds the literal: the
    // cycle is made of blocks counted whatever their type, with no counted type in it.
    assert!(has_closures(&c), "{c}");
    assert!(has(&c, "`App` interface values"), "{c}");
}

#[test]
fn a_closure_kept_in_an_array_it_captures_closes_a_cycle() {
    let c = cycle_capable(
        "function main() {
           const handlers: (() => number)[] = [];
           const f = (): number => handlers.length;
           handlers.push(f);
           console.log(handlers[0]());
         }",
    );
    assert!(has_closures(&c), "{c}");
}

#[test]
fn a_parent_pointer_closes_a_cycle() {
    let c = cycle_capable(
        "class TreeNode {
           parent: TreeNode | null = null;
           children: TreeNode[] = [];
           add(c: TreeNode) { c.parent = this; this.children.push(c); }
         }
         function main() {
           const root = new TreeNode();
           const kid = new TreeNode();
           root.add(kid);
           console.log(root.children.length, kid.parent === root);
         }",
    );
    assert!(has(&c, "class TreeNode"), "{c}");
}

#[test]
fn a_weak_back_reference_does_not_close_a_cycle() {
    // A connection holding its streams, each stream referring back to it: a cycle, unless the
    // back reference is `weak` (a weak field is no edge, velt_vir `weak_fields`). A type that
    // holds values of its own type (a tree's `children: Node[]`) stays cycle-capable whatever its
    // parent pointer is: a counted node can be put among its own children.
    let src = |back: &str| {
        format!(
            "class Stream {{
               conn: {back} = null;
               id: number;
               constructor(id: number) {{ this.id = id; }}
             }}
             class Conn {{
               streams: Stream[] = [];
               open(id: number): Stream {{
                 const s = new Stream(id);
                 s.conn = this;
                 this.streams.push(s);
                 return s;
               }}
             }}
             function main() {{
               const c = new Conn();
               const s = c.open(1);
               const back = s.conn;
               console.log(c.streams.length, back === c);
             }}"
        )
    };
    let strong = cycle_capable(&src("Conn | null"));
    assert!(
        has(&strong, "class Conn") && has(&strong, "class Stream"),
        "{strong}"
    );
    let out = debug_out(&src("weak Conn | null"));
    let line = (out.lines())
        .find_map(|l| l.strip_prefix("velt: cycle-capable types:"))
        .unwrap_or("");
    let line = line.trim();
    assert!(
        !has(line, "class Conn") && !has(line, "class Stream"),
        "{out}"
    );
}

#[test]
fn a_listener_capturing_its_view_weakly_does_not_close_a_cycle() {
    // The listener of `a_listener_closure_capturing_its_view_closes_a_cycle`, capturing the view
    // through a `weak` variable: the closure holds the variable's slot, no edge (velt_vir
    // weak_captures.rs, `env_captures`).
    let out = debug_out(
        "class Button {
           listeners: (() => void)[] = [];
           onClick(f: () => void) { this.listeners.push(f); }
           click() { for (const f of this.listeners) f(); }
         }
         class View { clicks = 0; button = new Button(); }
         function main() {
           const v = new View();
           const b = v.button;
           const me: weak View | null = v;
           b.onClick(() => { const s = me; if (s !== null) { s.clicks += 1; } });
           b.click();
           console.log(v.clicks);
         }",
    );
    let line = (out.lines())
        .find_map(|l| l.strip_prefix("velt: cycle-capable types:"))
        .unwrap_or("")
        .trim();
    assert!(
        !has(line, "class View") && !has(line, "class Button"),
        "{out}"
    );
}

#[test]
fn a_weak_map_value_capturing_its_key_weakly_is_not_traced() {
    // A weak map value whose closure refers to the key only through a `weak` variable cannot
    // lead back to it, so the map has no value trace (`weakMapNew`'s last argument 0) and its
    // entries are never examined (velt_vir weak_captures.rs, `env_owned`). With a strong
    // capture of the key, the map traces its values.
    let src = |decl: &str| {
        format!(
            "class Key {{ n: number; constructor(n: number) {{ this.n = n; }} }}
             class Holder {{ f: () => number; constructor(f: () => number) {{ this.f = f; }} }}
             function main() {{
               const cache = new WeakMap<Key, Holder>();
               const k = new Key(1);
               const w: {decl} = k;
               cache.set(k, new Holder(() => w?.n ?? 0));
               console.log(cache.get(k)!.f(), k.n);
             }}"
        )
    };
    let traced = |vir: &str| {
        let lines: Vec<&str> = vir.lines().collect();
        let at = (lines.iter())
            .position(|l| l.contains("weakMapNew("))
            .unwrap_or_else(|| panic!("no weakMapNew call:\n{vir}"));
        // `weakMapNew(keyTrace, retain, release, trace)`: the trace's operand, set just before
        // the call in the same block.
        let args = lines[at].split("weakMapNew(").nth(1).unwrap_or("");
        let arg = args
            .split(')')
            .next()
            .unwrap_or("")
            .rsplit(", ")
            .next()
            .unwrap_or("");
        let set = format!("{} = cast ", arg.trim());
        let value = (lines[..at].iter().rev())
            .find_map(|l| l.trim().strip_prefix(set.as_str()))
            .unwrap_or_else(|| panic!("no value for {arg}:\n{vir}"));
        !value.starts_with("0_ptr")
    };
    assert!(traced(&vir_out(&src("Key | null"))));
    assert!(!traced(&vir_out(&src("weak Key | null"))));
}

#[test]
fn counted_types_that_cannot_reach_themselves_are_not_cycle_capable() {
    let c = cycle_capable(
        "class Leaf { n: number = 1; }
         class Holder { leaf: Leaf; tags: string[] = []; constructor(l: Leaf) { this.leaf = l; } }
         function main() {
           const l = new Leaf();
           const a = new Holder(l);
           const b = new Holder(l);
           const both = [a, a, b];
           const f = (): number => l.n;
           console.log(both.length, a.leaf === b.leaf, f());
         }",
    );
    assert_eq!(c, "", "nothing here can form a cycle");
}

#[test]
fn a_program_that_shares_nothing_has_none() {
    let c = cycle_capable(
        "class TreeNode {
           left: TreeNode | null = null;
           right: TreeNode | null = null;
         }
         function make(d: number): TreeNode {
           const n = new TreeNode();
           if (d > 0) { n.left = make(d - 1); n.right = make(d - 1); }
           return n;
         }
         function count(n: TreeNode | null): number {
           return n === null ? 0 : 1 + count(n.left) + count(n.right);
         }
         function main() { console.log(count(make(4))); }",
    );
    assert_eq!(c, "", "uniquely owned trees are never in a cycle");
}

#[test]
fn a_uniquely_owned_object_on_the_path_does_not_hide_the_cycle() {
    // `Owner` is shared; `Part` has one owner but refers back to an `Owner`.
    let c = cycle_capable(
        "class Owner { part: Part = new Part(); n: number = 0; }
         class Part { back: Owner | null = null; }
         function main() {
           const o = new Owner();
           const keep = [o, o];
           o.part.back = o;
           console.log(keep.length, o.part.back === o);
         }",
    );
    assert!(has(&c, "class Owner"), "{c}");
}

#[test]
fn a_cycle_through_a_subclass_is_found_from_the_base_class() {
    let c = cycle_capable(
        "class Item { name: string = \"i\"; }
         class Linked extends Item { owner: Box | null = null; }
         class Box { item: Item; constructor(i: Item) { this.item = i; } }
         function main() {
           const l = new Linked();
           const b = new Box(l);
           l.owner = b;
           const keep = [b, b];
           console.log(keep.length, b.item.name);
         }",
    );
    assert!(has(&c, "class Box"), "{c}");
}

#[test]
fn a_cycle_through_an_interface_value_is_found() {
    let c = cycle_capable(
        "interface Listener { fire(): void; }
         class Counter implements Listener {
           view: View;
           constructor(v: View) { this.view = v; }
           fire() { this.view.n += 1; }
         }
         class View { n: number = 0; listeners: Listener[] = []; }
         function main() {
           const v = new View();
           const l: Listener = new Counter(v);
           v.listeners.push(l);
           for (const x of v.listeners) x.fire();
           console.log(v.n);
         }",
    );
    assert!(has(&c, "class View"), "{c}");
    assert!(has(&c, "class Counter"), "{c}");
}

#[test]
fn a_weak_map_value_that_refers_back_to_its_key_closes_a_cycle() {
    // The entry keeps the value while the key lives, and the value holds the key: the
    // ephemeron edge closes the cycle a collector must see.
    let c = cycle_capable(
        "class Raw { n: number = 1; }
         class Wrapper { target: Raw; constructor(t: Raw) { this.target = t; } }
         class Plain { v: number = 2; }
         function main() {
           const m = new WeakMap<Raw, Wrapper>();
           const r = new Raw();
           m.set(r, new Wrapper(r));
           const p = new Plain();
           const keep = [p, p];
           console.log(m.has(r), keep.length);
         }",
    );
    assert!(has(&c, "class Raw"), "{c}");
    assert!(has(&c, "class Wrapper"), "{c}");
    assert!(!has(&c, "class Plain"), "{c}");
}

#[test]
fn a_class_hierarchy_alone_is_not_a_cycle() {
    // A field of the base class type may hold a subclass object: that is "is a", not "owns",
    // so a hierarchy whose classes refer to nothing has no cycle.
    let c = cycle_capable(
        "class Shape { name: string = \"s\"; }
         class Circle extends Shape { r: number = 1; }
         class Square extends Shape { side: number = 2; }
         class Scene { main: Shape; constructor(s: Shape) { this.main = s; } }
         function main() {
           const s = new Scene(new Circle());
           const t = new Scene(new Square());
           const keep = [s, s, t];
           const both: Shape[] = [s.main, t.main, s.main];
           console.log(keep.length, both.length, both[0].name);
         }",
    );
    assert_eq!(c, "", "nothing here can form a cycle");
}

// Step 2: closures told apart by the function type they were made at, interface values by
// their interface.

/// A listener type that may throw: a closure that doesn't is wrapped to fit it (#2688), and the
/// wrapper captures the closure.
const LISTENER: &str = "type Listener = (n: number) => void throws unknown;";

#[test]
fn a_wrapper_alone_does_not_make_closures_cycle_capable() {
    let c = cycle_capable(&format!(
        "{LISTENER}
         function main() {{
           const ls: Listener[] = [];
           const f = (n: number) => {{ console.log(n); }};
           ls.push(f);
           const g = (n: number) => {{ console.log(n + 1); }};
           ls.push(g);
           for (const l of ls) l(1);
           console.log(ls.length);
         }}"
    ));
    assert!(!has_closures(&c), "{c}");
}

#[test]
fn a_cycle_through_a_wrapper_is_still_found() {
    // The view holds the wrapper, the wrapper the closure, and the closure the view.
    let c = cycle_capable(&format!(
        "{LISTENER}
         class View {{ n: number = 0; listeners: Listener[] = []; }}
         function main() {{
           const v = new View();
           const keep = [v, v];
           const f = (n: number) => {{ v.n += n; }};
           v.listeners.push(f);
           for (const l of v.listeners) l(1);
           console.log(keep.length, v.n);
         }}"
    ));
    assert!(has(&c, "class View"), "{c}");
    assert!(has_closures(&c), "{c}");
}

#[test]
fn a_closure_of_another_function_type_does_not_close_a_cycle() {
    // `View` holds closures of `() => void`; the closure capturing a `View` is a
    // `(n: number) => number`, which no `View` can hold.
    let c = cycle_capable(
        "class View {
           n: number = 0;
           onTick: (() => void)[] = [];
         }
         function main() {
           const v = new View();
           const keep = [v, v];
           let ticks = 0;
           v.onTick.push(() => { ticks += 1; });
           const scale = (n: number): number => n * v.n;
           for (const t of v.onTick) t();
           console.log(keep.length, scale(2), ticks);
         }",
    );
    assert!(!has(&c, "class View"), "{c}");
}

#[test]
fn an_interface_value_reaches_only_its_own_implementations() {
    // `Holder` holds `Shape`s; only a `Listener` refers back to a `Holder`.
    let c = cycle_capable(
        "interface Shape { area(): number; }
         interface Listener { fire(): void; }
         class Square implements Shape { s: number = 2; area(): number { return this.s * this.s; } }
         class Holder { shapes: Shape[] = []; n: number = 0; }
         class Watch implements Listener {
           h: Holder;
           constructor(h: Holder) { this.h = h; }
           fire() { this.h.n += 1; }
         }
         function main() {
           const h = new Holder();
           const keep = [h, h];
           h.shapes.push(new Square());
           const w: Listener = new Watch(h);
           w.fire();
           console.log(keep.length, h.shapes[0].area(), h.n);
         }",
    );
    assert!(!has(&c, "class Holder"), "{c}");
}

#[test]
fn each_cycle_capable_type_has_its_sites() {
    let out = debug_out(
        "class TreeNode {
           parent: TreeNode | null = null;
           children: TreeNode[] = [];
           add(c: TreeNode) { c.parent = this; this.children.push(c); }
         }
         function main() {
           const root = new TreeNode();
           const kid = new TreeNode();
           root.add(kid);
           console.log(root.children.length);
         }",
    );
    let line = out
        .lines()
        .find_map(|l| l.strip_prefix("velt: cycle-capable sites: class TreeNode: "))
        .unwrap_or_else(|| panic!("no sites for TreeNode:\n{out}"));
    // The two `new TreeNode()` of `main` (lines 7 and 8), and nothing else.
    assert_eq!(line.matches("main.velt:").count(), 2, "{line}");
    assert!(
        line.contains("main.velt:7:") && line.contains("main.velt:8:"),
        "{line}"
    );
}

#[test]
fn a_function_holding_itself_as_a_property_closes_a_cycle() {
    // #1780: the property table of `f`'s environment holds `f` as an `any`.
    let c = cycle_capable(
        "function make(): number {
           const f: any = function () { return 1; };
           f.self = f;
           return 1;
         }
         function main() { console.log(make()); }",
    );
    assert!(has_closures(&c), "{c}");
}

// From the review of #3122: the edges a weak collection's subclass, a generator object and a
// weak map key of a wider static type own.

#[test]
fn a_weak_map_subclass_keeps_its_own_fields() {
    let c = cycle_capable(
        "class Key { k = 1; }
         class Owner { cache: Cache | null = null; }
         class Cache extends WeakMap<Key, number> { owner: Owner | null = null; }
         function make(): void { const o = new Owner(); const c = new Cache(); c.owner = o; o.cache = c; }
         function main() { make(); console.log(1); }",
    );
    assert!(has(&c, "class Owner"), "{c}");
}

#[test]
fn a_generator_owns_its_state() {
    let c = cycle_capable(
        "class Holder { it: Generator<number, void, unknown> | null = null; n = 3; }
         function* count(h: Holder): Generator<number, void, unknown> {
           for (let i = 0; i < h.n; i++) yield i;
         }
         function make(): void { const h = new Holder(); h.it = count(h); }
         function main() { make(); console.log(1); }",
    );
    assert!(has(&c, "class Holder"), "{c}");
}

#[test]
fn an_async_generator_owns_its_state() {
    let c = cycle_capable(
        "class Holder { it: AsyncGenerator<number, void, unknown> | null = null; n = 3; }
         async function* count(h: Holder): AsyncGenerator<number, void, unknown> {
           for (let i = 0; i < h.n; i++) yield i;
         }
         function make(): void { const h = new Holder(); h.it = count(h); }
         async function main() { make(); console.log(1); }",
    );
    assert!(has(&c, "class Holder"), "{c}");
}

#[test]
fn an_object_key_leads_to_its_entries_from_its_concrete_type() {
    // Each key is kept alive only by the other map's value, as `object` keys.
    let c = cycle_capable(
        "class A { n = 1; }
         class B { n = 2; }
         function main() {
           const m1 = new WeakMap<object, B>();
           const m2 = new WeakMap<object, A>();
           const a = new A();
           const b = new B();
           m1.set(a, b);
           m2.set(b, a);
           console.log(m1.has(a), m2.has(b));
         }",
    );
    assert!(has(&c, "class A"), "{c}");
    assert!(has(&c, "class B"), "{c}");
}

#[test]
fn a_base_class_key_leads_to_its_entries_from_a_subclass() {
    // The key is a `Leaf`, typed as its base; the entry's value holds the `Leaf`.
    let c = cycle_capable(
        "class Base { n = 1; }
         class Leaf extends Base { m = 2; }
         class Tag { of: Leaf; constructor(l: Leaf) { this.of = l; } }
         function main() {
           const m = new WeakMap<Base, Tag>();
           const l = new Leaf();
           const b = new Base();
           m.set(l, new Tag(l));
           m.set(b, new Tag(new Leaf()));
           console.log(m.has(l), m.has(b));
         }",
    );
    assert!(has(&c, "class Tag"), "{c}");
}

#[test]
fn an_interface_key_leads_to_its_entries_from_its_implementation() {
    let c = cycle_capable(
        "interface Keyed { id(): number; }
         class Item implements Keyed { n = 1; id(): number { return this.n; } }
         class Note { item: Item; constructor(i: Item) { this.item = i; } }
         function main() {
           const m = new WeakMap<Keyed, Note>();
           const i = new Item();
           const k: Keyed = i;
           m.set(k, new Note(i));
           console.log(m.has(k));
         }",
    );
    assert!(has(&c, "class Note"), "{c}");
}

#[test]
fn interface_values_have_the_sites_of_the_values_they_hold() {
    // The literal is held as an `App` interface value: its site is that entry's too.
    let out = debug_out(
        "interface App { name: string; use(p: (a: App) => void): App; }
         function makeApp(name: string): App {
           const app: App = { name, use(p) { p(app); return app; } };
           return app;
         }
         function main() { makeApp(\"x\").use((x) => console.log(x.name)); }",
    );
    let line = out
        .lines()
        .find_map(|l| l.strip_prefix("velt: cycle-capable sites: `App` interface values: "))
        .unwrap_or_else(|| panic!("no sites for `App` interface values:\n{out}"));
    assert!(line.contains("main.velt:3:"), "{line}");
}

#[test]
fn an_abort_error_keeps_errors_out_of_the_set() {
    // An `AbortError`'s cause is the string reason `abort(reason)` gave (std/task/errors.velt),
    // not a dynamic value that could hold the error back: a program that throws one from
    // `throwIfAborted` and reads its reason has no `Error` (nor dynamic values) on a cycle, so
    // none of its errors pays for the collector (#3119).
    let out = debug_out(
        "import { AbortError } from \"velt/task\";
         function main() {
           const c = new AbortController();
           c.abort(\"stop\");
           try {
             c.signal.throwIfAborted();
           } catch (e) {
             if (e instanceof AbortError) {
               console.log(e.message, e.cause ?? \"none\");
             }
           }
           console.log(c.signal.reason);
         }",
    );
    let line = (out.lines())
        .find_map(|l| l.strip_prefix("velt: cycle-capable types:"))
        .unwrap_or("")
        .trim();
    assert!(!has(line, "class std/prelude/errors::Error"), "{out}");
    assert!(!has(line, "dynamic values"), "{out}");
}
