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
