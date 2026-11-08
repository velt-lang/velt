//! Which std functions the cache serves (hand-built VIR).

use super::statics::Names;
use super::*;
use velt_vir::vir::{
    BasicBlock, FuncId, Function, Local, Place, Program, StaticData, StaticId, Ty,
};

/// A std function `name` of `stmts` statements, then a call of each of `calls` (by index), and
/// `extra` as its last statement.
fn func(name: &str, stmts: usize, calls: &[u32], extra: Option<Rvalue>) -> Function {
    let symbol = velt_vir::mangle::mangle(&format!("std/m::{name}"));
    let mut f = Function::new(symbol, vec![], Ty::I64, Linkage::Internal);
    f.locals.push(vir::LocalDecl::new(Ty::I64, None));
    f.locals.push(vir::LocalDecl::new(Ty::Ptr, None));
    let set = |v: i128| {
        Stmt::Assign(
            Place::local(Local(0)),
            Rvalue::Use(Operand::Const(Const::Int(v), Ty::I64)),
        )
    };
    let mut stmts: Vec<Stmt> = (0..stmts as i128).map(set).collect();
    if let Some(rv) = extra {
        stmts.push(Stmt::Assign(Place::local(Local(1)), rv));
    }
    let mut blocks = vec![];
    for (n, &c) in calls.iter().enumerate() {
        blocks.push(BasicBlock {
            stmts: std::mem::take(&mut stmts),
            term: Terminator::Call {
                callee: Callee::Func(FuncId(c)),
                args: vec![],
                dest: Some(Place::local(Local(0))),
                next: vir::BlockId(n as u32 + 1),
            },
        });
    }
    blocks.push(BasicBlock {
        stmts,
        term: Terminator::Return(Operand::Copy(Place::local(Local(0)))),
    });
    f.blocks = blocks;
    f
}

fn names(p: &Program) -> Names {
    Names::new(
        p,
        &velt_vir::fingerprint::Canon::new(p, velt_vir::fingerprint::Mode::Link),
    )
}

fn cands(p: &Program) -> Vec<usize> {
    candidates(p, &names(p), Rule::Release)
}

fn reasons_of(p: &Program) -> BTreeMap<&'static str, usize> {
    reasons(p, &names(p), Rule::Release)
}

fn program(funcs: Vec<Function>, statics: Vec<StaticData>) -> Program {
    Program {
        funcs,
        statics,
        ..Program::default()
    }
}

fn stat(relocs: bool) -> StaticData {
    StaticData {
        bytes: vec![0; 8],
        align: 8,
        relocs: if relocs {
            vec![(0, Const::Func(FuncId(0)))]
        } else {
            vec![]
        },
        mutable: None,
        ..Default::default()
    }
}

fn addr(s: u32) -> Option<Rvalue> {
    Some(Rvalue::Use(Operand::Const(
        Const::Static(StaticId(s)),
        Ty::Ptr,
    )))
}

#[test]
fn large_std_functions_are_served_small_ones_stay() {
    // `keys::disambiguate` appends the suffix to the mangled symbol.
    let mut dup = func("big", 50, &[], None);
    dup.symbol.push_str("$dup2");
    let p = program(
        vec![
            func("big", 50, &[1], None),
            func("small", 3, &[], None),
            dup,
        ],
        vec![],
    );
    assert_eq!(cands(&p), [0]);
    assert_eq!(
        inlinable(&p, 0),
        [0, 1],
        "the small callee is compiled into it"
    );
    let by = reasons_of(&p);
    assert_eq!((by["small"], by["$dup"]), (4, 51));
}

/// A static with identity (relocations: a vtable, a type descriptor; or written) is served by
/// name, also when only a small function it may inline names it; what its relocations point
/// at is reached too. Two statics with the same contents can't be named, so their users stay.
#[test]
fn statics_with_identity_are_served_by_name_unless_two_are_alike() {
    let p = program(
        vec![
            func("plain", 50, &[], addr(0)),
            func("table", 50, &[], addr(1)),
            func("through_small", 50, &[3], None),
            func("small", 3, &[], addr(1)),
        ],
        vec![stat(false), stat(true)],
    );
    assert_eq!(cands(&p), [0, 1, 2]);
    let mut written = p.clone();
    written.statics[0].mutable = Some(7);
    assert_eq!(cands(&written), [0, 1, 2], "a written word is named too");
    assert_eq!(super::statics::reached(&p, &inlinable(&p, 2)), [1]);
    // A second vtable with the same contents: neither can be named.
    let mut alike = p.clone();
    alike.statics.push(stat(true));
    alike.funcs.push(func("other", 50, &[], addr(2)));
    assert_eq!(cands(&alike), [0]);
    assert!(reasons_of(&alike).contains_key("statics with identity"));
    // What a named static points at must be nameable too.
    let mut nested = p.clone();
    nested.statics[1].relocs = vec![(0, Const::Static(StaticId(2)))];
    nested.statics.push(stat(false));
    nested.statics.push(stat(false));
    assert_eq!(super::statics::reached(&nested, &[1]), [1, 2]);
    assert_eq!(
        cands(&nested),
        [0],
        "the plain static it points at has a twin"
    );
}

/// A function that calls through a pointer (a callback) stays where `const_fields` can
/// specialize it for its caller's closure.
#[test]
fn callback_callers_stay_in_the_program() {
    let mut f = func("each", 50, &[], None);
    f.blocks[0].term = Terminator::Call {
        callee: Callee::Ptr {
            target: Operand::Copy(Place::local(Local(1))),
            params: vec![],
            ret: Ty::I64,
        },
        args: vec![],
        dest: Some(Place::local(Local(0))),
        next: vir::BlockId(1),
    };
    f.blocks.push(BasicBlock {
        stmts: vec![],
        term: Terminator::Return(Operand::Copy(Place::local(Local(0)))),
    });
    let p = program(vec![f], vec![]);
    assert!(cands(&p).is_empty());
    assert!(reasons_of(&p).contains_key("callbacks"));
}

/// `inline()` makes a function of any size part of what its callers' objects inline, and in a
/// release build keeps it in the program, as a small one is.
#[test]
fn a_function_marked_inline_counts_as_small_whatever_its_size() {
    let mut marked = func("marked", 200, &[], None);
    marked.inline = Inline::Always;
    let caller = func("caller", 60, &[1], None);
    let plain = func("plain", 200, &[], None);
    let caller_of_plain = func("caller_of_plain", 60, &[2], None);
    let p = program(vec![caller, marked, plain, caller_of_plain], vec![]);
    assert_eq!(inlinable(&p, 0), [0, 1], "followed");
    assert_eq!(inlinable(&p, 3), [3], "a large unmarked function is not");
    assert_eq!(declared(&p, &inlinable(&p, 3)), [2]);
    assert_eq!(cands(&p), [0, 2, 3], "stays where callers can inline it");
}

/// A debug build inlines only its helpers, so it serves small and marked functions too.
#[test]
fn a_debug_build_serves_small_functions_but_not_what_it_inlines() {
    let small = func("small", 3, &[], None);
    let helper = func("helper", 3, &[], None);
    let mut marked = func("marked", 3, &[], None);
    marked.inline = Inline::Always;
    let p = program(vec![small, helper, marked], vec![]);
    let helpers = [false, true, false];
    let rule = Rule::Debug {
        helpers: &helpers,
        thread_data: true,
        user_sites: &[],
    };
    assert_eq!(candidates(&p, &names(&p), rule), [0, 2]);
    assert_eq!(cands(&p), Vec::<usize>::new(), "release keeps all three");
}

/// Where a program's thread-local values are on thread keys, an object has no way to reach them:
/// what reads one stays in the program, and so does what would inline that.
#[test]
fn code_reaching_a_thread_local_stays_where_it_has_no_data_to_share() {
    let symbol = velt_vir::tls::TlsCall::Get {
        id: 1,
        bits: 64,
        init: 0,
    }
    .symbol();
    let mut reader = func("reader", 3, &[], None);
    reader.blocks.insert(
        0,
        BasicBlock {
            stmts: vec![],
            term: Terminator::Call {
                callee: Callee::Extern(vir::ExternId(0)),
                args: vec![],
                dest: Some(Place::local(Local(0))),
                next: vir::BlockId(1),
            },
        },
    );
    let caller = func("caller", 3, &[0], None);
    let other = func("other", 3, &[], None);
    let mut p = program(vec![reader, caller, other], vec![]);
    p.externs.push(vir::ExternFn {
        symbol,
        params: vec![],
        ret: Ty::I64,
        noreturn: false,
        host: None,
        c_decl: None,
    });
    let helpers = [false; 3];
    let rule = |thread_data| Rule::Debug {
        helpers: &helpers,
        thread_data,
        user_sites: &[],
    };
    assert_eq!(candidates(&p, &names(&p), rule(true)), [0, 1, 2]);
    assert_eq!(candidates(&p, &names(&p), rule(false)), [2]);
}

/// A string's header static, pointing at its text as a location's does.
fn header(text: u32) -> StaticData {
    StaticData {
        bytes: vec![0; 8],
        align: 8,
        relocs: vec![(0, Const::Static(StaticId(text)))],
        ..Default::default()
    }
}

fn text(s: &str) -> StaticData {
    StaticData {
        bytes: s.as_bytes().to_vec(),
        align: 1,
        ..Default::default()
    }
}

/// A caller-tracking instance of a call in the program's own file holds that path: no other
/// program has its key. It stays for Cranelift, whose code any program shares, and so does what
/// would inline it. One for a call in std, or in code a std generator wrote, has the same key in
/// every program. `Program::files` stays empty, as in a build without line information.
#[test]
fn code_showing_a_place_in_the_program_stays_in_the_program() {
    let site = func("set_at_user_call", 3, &[], addr(1));
    let caller = func("constructor_at_user_call", SMALL, &[0], None);
    let in_std = func("set_at_std_call", 3, &[], addr(3));
    let generated = func("set_at_generated_call", 3, &[], addr(5));
    let plain = func("other", 3, &[], None);
    let on_windows = func("set_at_windows_call", 3, &[], addr(7));
    let p = program(
        vec![site, caller, in_std, generated, plain, on_windows],
        vec![
            text(" at /home/me/app.velt:3:3"),
            header(0),
            text(" at std/prelude/map.velt:117:24"),
            header(2),
            text(" at <generated>/std/json/reader.velt:1:2"),
            header(4),
            text(" at C:/Users/me/app.velt:3:3"),
            header(6),
        ],
    );
    assert!(p.files.is_empty());
    let helpers = [false; 6];
    let sites = user_sites(&p);
    assert_eq!(
        sites,
        [true, false, false, false, false, false, true, false]
    );
    let rule = Rule::Debug {
        helpers: &helpers,
        thread_data: true,
        user_sites: &sites,
    };
    assert_eq!(candidates(&p, &names(&p), rule), [2, 3, 4]);
}

/// Only a text of the form a location has names a file, whatever colons its path holds.
#[test]
fn a_location_is_a_path_a_line_and_a_column() {
    fn path(text: &str) -> Option<&str> {
        site_path(text.as_bytes())
    }
    assert_eq!(path(" at a/b.velt:3:14"), Some("a/b.velt"));
    assert_eq!(path(" at C:/a b/c.velt:3:14"), Some("C:/a b/c.velt"));
    assert_eq!(path(" at C:\\a\\c.velt:3:14"), Some("C:\\a\\c.velt"));
    assert_eq!(path(" at //?/C:/a/c.velt:3:14"), Some("//?/C:/a/c.velt"));
    assert_eq!(
        path(" at \\\\?\\C:\\c.velt:3:14"),
        Some("\\\\?\\C:\\c.velt")
    );
    assert_eq!(
        path(" at //server/share/c.velt:3:14"),
        Some("//server/share/c.velt")
    );
    assert_eq!(path("a/b.velt:3:14"), None, "no ` at `");
    assert_eq!(path(" at 12:30"), None, "no path");
    assert_eq!(
        path(" at C:/a/c.velt"),
        None,
        "a drive's colon is no number"
    );
    assert_eq!(path(" at a:b:14"), None);
    assert_eq!(path(" at a:3:"), None);
    assert_eq!(path("no location"), None);
    assert_eq!(site_path(&[0xff, b':', b':']), None);
}

/// A release build's default serves only what std marks `noInline()`, at any size, with the small
/// functions it calls compiled into it; a large function without the marker, or one marked
/// `inline()`, stays where the optimizer can inline it. A `noInline()` function is never
/// inlined, so it is in no other function's inlinable set.
#[test]
fn by_default_a_release_build_serves_only_what_is_never_inlined() {
    let mut never_large = func("neverLarge", 60, &[2, 4], None);
    never_large.inline = Inline::Never;
    let mut never_small = func("neverSmall", 2, &[], None);
    never_small.inline = Inline::Never;
    let mut always = func("always", 60, &[], None);
    always.inline = Inline::Always;
    let p = program(
        vec![
            func("large", 60, &[0], None),
            never_large,
            func("small", 2, &[], None),
            always,
            never_small,
        ],
        vec![],
    );
    assert!(has_no_inline(&p));
    assert_eq!(candidates(&p, &names(&p), Rule::NoInline), [1, 4]);
    assert_eq!(
        cands(&p),
        [0, 1, 4],
        "every large function, without the default"
    );
    assert_eq!(
        inlinable(&p, 1),
        [1, 2],
        "never itself, nor what is marked so"
    );
    let by = reasons(&p, &names(&p), Rule::NoInline);
    assert_eq!(by.get("inlinable"), Some(&(62 + 3 + 61)));
    assert!(!has_no_inline(&program(
        vec![func("large", 60, &[], None)],
        vec![]
    )));
}
