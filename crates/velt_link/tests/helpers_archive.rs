//! A link against a shared runtime with a helpers archive (`LinkRequest::helpers_lib`, #3315):
//! what the program's objects call and the shared runtime does not export comes from the archive
//! named after it, and nothing else does. Real links on the host, with stand-ins compiled here
//! with `rustc`: a shared library under the shared runtime's name exporting one function, an
//! archive with two members (one defines the other function the program calls, one defines the
//! shared library's function again, as the static runtime defines what the shared one exports),
//! and a Cranelift-built object whose `main` calls both functions. The program must call the
//! shared library's function: an archive named before the shared runtime would serve its own.
//!
//! On arm64 Linux the archive is the static runtime and the functions are the outline-atomics
//! helpers LLVM's code calls (`__aarch64_cas4_acq`, …): `veltc`'s prebuilt-object tests link
//! those there.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use cranelift_codegen::ir::{types, AbiParam, InstBuilder};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

/// The shared runtime's stand-in exports this.
const SHARED_SRC: &str = "#[no_mangle] pub extern \"C\" fn velt_shared_part() -> i32 { 30 }\n";
/// The helpers archive's stand-in defines this; the shared library does not.
const HELPERS_SRC: &str = "extern crate velt_dup;\n\
     #[no_mangle] pub extern \"C\" fn velt_helper_part() -> i32 { 12 }\n";
/// Another member of the archive (a crate of its own, so an object of its own) defines the shared
/// library's function with another result: a link that takes it has taken a runtime member.
const DUP_SRC: &str = "#[no_mangle] pub extern \"C\" fn velt_shared_part() -> i32 { 99 }\n";

/// Triple for the hand-built object: Cranelift records a Mach-O platform only for `macosx`
/// triples, and Apple's linker rejects objects without one.
fn object_triple(triple: &str) -> String {
    match triple.strip_suffix("-apple-darwin") {
        Some("x86_64") => "x86_64-apple-macosx10.12".to_string(),
        Some(arch) => format!("{arch}-apple-macosx11.0"),
        None => triple.to_string(),
    }
}

/// `int main(void) { return velt_shared_part() + velt_helper_part(); }`
fn build_object(triple: &str) -> Vec<u8> {
    let mut flags = settings::builder();
    flags.set("is_pic", "true").unwrap();
    let isa = cranelift_codegen::isa::lookup_by_name(triple)
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    let builder =
        ObjectBuilder::new(isa, "main", cranelift_module::default_libcall_names()).unwrap();
    let mut module = ObjectModule::new(builder);
    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(types::I32));
    let import = |module: &mut ObjectModule, name: &str| {
        module
            .declare_function(name, Linkage::Import, &sig)
            .unwrap()
    };
    let shared = import(&mut module, "velt_shared_part");
    let helper = import(&mut module, "velt_helper_part");
    let main = module
        .declare_function("main", Linkage::Export, &sig)
        .unwrap();
    let mut ctx = module.make_context();
    ctx.func.signature = sig.clone();
    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let block = b.create_block();
        b.switch_to_block(block);
        b.seal_block(block);
        let shared = module.declare_func_in_func(shared, b.func);
        let helper = module.declare_func_in_func(helper, b.func);
        let a = b.ins().call(shared, &[]);
        let a = b.inst_results(a)[0];
        let c = b.ins().call(helper, &[]);
        let c = b.inst_results(c)[0];
        let sum = b.ins().iadd(a, c);
        b.ins().return_(&[sum]);
        b.finalize();
    }
    module.define_function(main, &mut ctx).unwrap();
    module.finish().emit().unwrap()
}

/// Compile `src` as the crate `name` of `crate_type` to `out`; crates in `dir` can be named.
fn rustc(crate_type: &str, name: &str, src: &str, dir: &Path, out: &Path) {
    let file = dir.join(format!("{name}.rs"));
    std::fs::write(&file, src).unwrap();
    let st = command(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--edition", "2021", "-O", "--crate-type", crate_type])
        .args(["--crate-name", name, "-L"])
        .arg(dir)
        .arg("-o")
        .arg(out)
        .arg(&file)
        .status()
        .expect("run rustc");
    assert!(st.success(), "rustc failed to build {name}");
}

/// Link with and without the archive through `link`, which names the linker in failures.
fn check(what: &str, dir: &Path, link: &dyn Fn(&velt_link::LinkRequest) -> Result<(), String>) {
    let triple = velt_link::host_triple();
    let os = velt_link::TargetOs::from_triple(&triple).unwrap();
    let shared = dir.join(velt_link::shared_runtime_lib_name(os));
    let helpers = dir.join(velt_link::runtime_lib_name(&triple));
    let objects = [dir.join("main.o")];
    let request = |exe: &'static str, helpers_lib| {
        (
            dir.join(exe),
            velt_link::LinkRequest {
                target: &triple,
                objects: &objects,
                runtime_lib: &shared,
                output: Path::new(""),
                release: false,
                native: &[],
                c_libs: &[],
                helpers_lib,
            },
        )
    };
    // Without the archive nothing defines the helper: the link fails and names it.
    let (exe, mut req) = request("without", None);
    req.output = &exe;
    let err = link(&req).expect_err("a link without the helpers archive");
    assert!(err.contains("velt_helper_part"), "{what}: {err}");
    // With it the program links and calls both: the shared library's 30, not the 99 of the
    // archive's member that defines the same function, and the archive's 12.
    let (exe, mut req) = request("with", Some(helpers.as_path()));
    req.output = &exe;
    link(&req).unwrap_or_else(|e| panic!("{what}: link with the helpers archive failed:\n{e}"));
    // cargo puts its own directories on the library path, where the real shared runtime of the
    // same name is: the loader looks there before the executable's rpath.
    let ran = command(&exe).env("LD_LIBRARY_PATH", dir).status().unwrap();
    assert_eq!(ran.code(), Some(42), "{what}: {}", exe.display());
}

#[test]
fn a_link_against_the_shared_runtime_takes_helpers_from_the_archive() {
    let triple = velt_link::host_triple();
    if triple.contains("musl") {
        eprintln!("skipped: musl links no shared runtime");
        return;
    }
    let os = velt_link::TargetOs::from_triple(&triple).unwrap();
    let dir: PathBuf =
        std::env::temp_dir().join(format!("velt_link_helpers_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let shared = dir.join(velt_link::shared_runtime_lib_name(os));
    rustc("cdylib", "velt_rt_shared", SHARED_SRC, &dir, &shared);
    rustc(
        "rlib",
        "velt_dup",
        DUP_SRC,
        &dir,
        &dir.join("libvelt_dup.rlib"),
    );
    let helpers = dir.join(velt_link::runtime_lib_name(&triple));
    rustc("staticlib", "velt_rt", HELPERS_SRC, &dir, &helpers);
    std::fs::write(dir.join("main.o"), build_object(&object_triple(&triple))).unwrap();

    // The system linker (`cc`).
    std::env::set_var("VELT_LINKER", "system");
    check("system linker", &dir, &|req| velt_link::link(req));
    std::env::remove_var("VELT_LINKER");

    // The bundled linker (ELF: where the kit has no archive with the helpers): a kit for the
    // host built here, with the Rust toolchain's `rust-lld`.
    if os == velt_link::TargetOs::Linux {
        match velt_link::bundled::find_lld() {
            None => eprintln!("skipped the bundled linker: no rust-lld"),
            Some(lld) => {
                let kit_dir = dir.join("kit");
                velt_link::kit::build::build(&velt_link::kit::build::Options {
                    target: &triple,
                    out: &kit_dir,
                    lld: &lld,
                    runtime: None,
                })
                .unwrap_or_else(|e| panic!("kit build failed:\n{e}"));
                let bundled = velt_link::bundled::Bundled {
                    lld,
                    kit: velt_link::kit::Kit::open(&kit_dir, &triple).unwrap(),
                };
                check("bundled linker", &dir, &|req| {
                    velt_link::link_bundled(req, &bundled)
                });
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[path = "../../../tests/common/command.rs"]
mod command;
use command::command;
