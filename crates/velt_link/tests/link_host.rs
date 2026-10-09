//! Real link on the host: a Cranelift-built object exporting `velt_main` + a stand-in runtime
//! staticlib (compiled here with `rustc`, implementing the few rt_abi.md symbols we call) → exe → run.
//! With the system linker, with the bundled one (the Rust toolchain's `rust-lld` and a kit built
//! here), and with a `$VELT_LINKER` program.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The tests set `$VELT_RT_LIB` / `$VELT_LINKER`; one at a time.
static ENV: Mutex<()> = Mutex::new(());

use cranelift_codegen::ir::{types, AbiParam, InstBuilder};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

const STANDIN_RT: &str = r#"
use std::io::Write;
use std::sync::Mutex;

static OUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

extern "C" { fn velt_main() -> i32; }

#[no_mangle]
pub extern "C" fn velt_rt_write_i64(_stream: u32, v: i64) {
    write!(OUT.lock().unwrap(), "{v}").unwrap();
}

#[no_mangle]
pub extern "C" fn velt_rt_write_byte(_stream: u32, b: u8) {
    OUT.lock().unwrap().push(b);
}

#[no_mangle]
pub extern "C" fn velt_rt_flush() {
    let mut buf = OUT.lock().unwrap();
    let mut out = std::io::stdout().lock();
    out.write_all(&buf).unwrap();
    out.flush().unwrap();
    buf.clear();
}

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let code = unsafe { velt_main() };
    velt_rt_flush();
    code
}
"#;

fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{arch}-pc-windows-msvc"),
        "macos" => format!("{arch}-apple-darwin"),
        _ if cfg!(target_env = "musl") => format!("{arch}-unknown-linux-musl"),
        _ => format!("{arch}-unknown-linux-gnu"),
    }
}

/// Triple for the hand-built object. Apple's linker rejects Mach-O objects without a platform,
/// and Cranelift only records one for `macosx` triples (`velt_codegen_cl` normalizes the same way).
fn object_triple(triple: &str) -> String {
    match triple.strip_suffix("-apple-darwin") {
        Some("x86_64") => "x86_64-apple-macosx10.12".to_string(),
        Some(arch) => format!("{arch}-apple-macosx11.0"),
        None => triple.to_string(),
    }
}

/// `int32_t velt_main(void) { write_i64(1, 42); write_byte(1, '\n'); flush(); return 7; }`
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
    let cc = module.isa().default_call_conv();

    let mut sig_i64 = module.make_signature();
    sig_i64.params.push(AbiParam::new(types::I32));
    sig_i64.params.push(AbiParam::new(types::I64));
    let write_i64 = module
        .declare_function("velt_rt_write_i64", Linkage::Import, &sig_i64)
        .unwrap();

    let mut sig_byte = module.make_signature();
    sig_byte.params.push(AbiParam::new(types::I32));
    sig_byte.params.push(AbiParam::new(types::I8).uext());
    let write_byte = module
        .declare_function("velt_rt_write_byte", Linkage::Import, &sig_byte)
        .unwrap();

    let sig_flush = module.make_signature();
    let flush = module
        .declare_function("velt_rt_flush", Linkage::Import, &sig_flush)
        .unwrap();

    let mut sig_main = module.make_signature();
    sig_main.returns.push(AbiParam::new(types::I32));
    let main = module
        .declare_function("velt_main", Linkage::Export, &sig_main)
        .unwrap();

    let mut ctx = module.make_context();
    ctx.func.signature = sig_main;
    ctx.func.signature.call_conv = cc;
    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let block = b.create_block();
        b.switch_to_block(block);
        b.seal_block(block);
        let fi = module.declare_func_in_func(write_i64, b.func);
        let fb = module.declare_func_in_func(write_byte, b.func);
        let ff = module.declare_func_in_func(flush, b.func);
        let one = b.ins().iconst(types::I32, 1);
        let v = b.ins().iconst(types::I64, 42);
        b.ins().call(fi, &[one, v]);
        let nl = b.ins().iconst(types::I8, 10);
        b.ins().call(fb, &[one, nl]);
        b.ins().call(ff, &[]);
        let seven = b.ins().iconst(types::I32, 7);
        b.ins().return_(&[seven]);
        b.finalize();
    }
    module.define_function(main, &mut ctx).unwrap();
    module.finish().emit().unwrap()
}

fn build_standin_rt(dir: &Path, triple: &str) -> PathBuf {
    let src = dir.join("standin_rt.rs");
    std::fs::write(&src, STANDIN_RT).unwrap();
    let out = dir.join(velt_link::runtime_lib_name(triple));
    let st = command(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args([
            "--edition",
            "2021",
            "--crate-type",
            "staticlib",
            "--crate-name",
            "velt_rt",
            "-O",
            "-o",
        ])
        .arg(&out)
        .arg(&src)
        .status()
        .expect("run rustc");
    assert!(st.success(), "rustc failed to build the stand-in runtime");
    out
}

/// A scratch directory with the stand-in runtime and the program object.
struct Setup {
    triple: String,
    dir: PathBuf,
    rt: PathBuf,
    obj: PathBuf,
}

impl Setup {
    fn new(name: &str) -> Setup {
        let triple = host_triple();
        let dir = std::env::temp_dir().join(format!("velt_link_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = build_standin_rt(&dir, &triple);
        let obj_ext = if cfg!(windows) { "obj" } else { "o" };
        let obj = dir.join(format!("main.{obj_ext}"));
        std::fs::write(&obj, build_object(&object_triple(&triple))).unwrap();
        Setup {
            triple,
            dir,
            rt,
            obj,
        }
    }

    fn request<'a>(
        &'a self,
        objects: &'a [PathBuf],
        exe: &'a Path,
        release: bool,
    ) -> velt_link::LinkRequest<'a> {
        velt_link::LinkRequest {
            target: &self.triple,
            objects,
            runtime_lib: &self.rt,
            output: exe,
            release,
            native: &[],
        }
    }
}

fn assert_runs(exe: &Path) {
    let o = command(exe).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "42\n",
        "{}",
        exe.display()
    );
    assert_eq!(o.status.code(), Some(7));
}

#[test]
fn link_and_run_on_host() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let Setup {
        triple,
        dir,
        rt,
        obj,
    } = Setup::new("host");
    std::env::set_var("VELT_LINKER", "system");
    // $VELT_RT_LIB is the first place find_runtime_lib looks.
    std::env::set_var("VELT_RT_LIB", &rt);
    let found = velt_link::find_runtime_lib(&triple).unwrap();
    std::env::remove_var("VELT_RT_LIB");
    assert_eq!(found, rt);

    for release in [false, true] {
        let exe = dir.join(format!("prog_{release}{}", std::env::consts::EXE_SUFFIX));
        let objects = [obj.clone()];
        velt_link::link(&velt_link::LinkRequest {
            target: &triple,
            objects: &objects,
            runtime_lib: &found,
            output: &exe,
            release,
            native: &[],
        })
        .unwrap_or_else(|e| panic!("link failed (release={release}):\n{e}"));
        assert_runs(&exe);
    }

    // A missing symbol must surface the linker's own error text.
    let bad_rt_src = dir.join("empty_rt.rs");
    std::fs::write(&bad_rt_src, "#[no_mangle] pub extern \"C\" fn unused() {}").unwrap();
    let bad_rt = dir.join(format!("bad_{}", velt_link::runtime_lib_name(&triple)));
    let st = command("rustc")
        .args(["--crate-type", "staticlib", "--crate-name", "bad", "-o"])
        .arg(&bad_rt)
        .arg(&bad_rt_src)
        .status()
        .unwrap();
    assert!(st.success());
    let objects = [obj.clone()];
    let err = velt_link::link(&velt_link::LinkRequest {
        target: &triple,
        objects: &objects,
        runtime_lib: &bad_rt,
        output: &dir.join(format!("bad{}", std::env::consts::EXE_SUFFIX)),
        release: false,
        native: &[],
    })
    .unwrap_err();
    assert!(
        err.contains("velt_rt_write_i64"),
        "linker error should name the missing symbol:\n{err}"
    );
    std::env::remove_var("VELT_LINKER");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bundled linker: a kit for the host built here, linked with the Rust toolchain's
/// `rust-lld` (what `velt` falls back to in a checkout).
#[test]
fn bundled_linker_links_and_runs() {
    let setup = Setup::new("bundled");
    let Some(lld) = velt_link::bundled::find_lld() else {
        eprintln!("skipped: no rust-lld");
        return;
    };
    let kit_dir = setup.dir.join("kit");
    velt_link::kit::build::build(&velt_link::kit::build::Options {
        target: &setup.triple,
        out: &kit_dir,
        lld: &lld,
        runtime: None,
    })
    .unwrap_or_else(|e| panic!("kit build failed:\n{e}"));
    let bundled = velt_link::bundled::Bundled {
        lld,
        kit: velt_link::kit::Kit::open(&kit_dir, &setup.triple).unwrap(),
    };
    let objects = [setup.obj.clone()];
    for release in [false, true] {
        let exe = setup
            .dir
            .join(format!("bundled_{release}{}", std::env::consts::EXE_SUFFIX));
        velt_link::link_bundled(&setup.request(&objects, &exe, release), &bundled)
            .unwrap_or_else(|e| panic!("bundled link failed (release={release}):\n{e}"));
        assert_runs(&exe);
    }
    let _ = std::fs::remove_dir_all(&setup.dir);
}

/// `$VELT_LINKER=<program>` runs that program with the system linker's arguments.
#[test]
fn velt_linker_names_the_linker_program() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let setup = Setup::new("override");
    let log = setup.dir.join("args.txt");
    // A wrapper that records its arguments, then links with the system linker (`cc`) on Unix;
    // on Windows it only records them (link.exe needs the environment velt finds for it).
    let wrapper = if cfg!(windows) {
        let w = setup.dir.join("linker.cmd");
        std::fs::write(
            &w,
            format!("@echo %* > \"{}\"\r\n@exit /b 1\r\n", log.display()),
        )
        .unwrap();
        w
    } else {
        let w = setup.dir.join("linker.sh");
        std::fs::write(
            &w,
            format!(
                "#!/bin/sh\necho \"$@\" > '{}'\nexec cc \"$@\"\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&w, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        w
    };
    std::env::set_var("VELT_LINKER", &wrapper);
    let objects = [setup.obj.clone()];
    let exe = setup
        .dir
        .join(format!("override{}", std::env::consts::EXE_SUFFIX));
    let result = velt_link::link(&setup.request(&objects, &exe, false));
    let found = velt_link::find_linker(&setup.triple);
    std::env::remove_var("VELT_LINKER");
    assert_eq!(found.unwrap(), wrapper);
    let args = std::fs::read_to_string(&log).expect("the $VELT_LINKER program ran");
    if cfg!(windows) {
        assert!(args.contains("/OUT:"), "{args}");
        assert!(result.unwrap_err().contains("linker.cmd"));
    } else {
        assert!(args.contains("-o"), "{args}");
        result.unwrap_or_else(|e| panic!("link through the wrapper failed:\n{e}"));
        assert_runs(&exe);
    }
    let _ = std::fs::remove_dir_all(&setup.dir);
}

#[path = "../../../tests/common/command.rs"]
mod command;
use command::command;
