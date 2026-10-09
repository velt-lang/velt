//! On macOS, executables record the shared runtime's install name, so it must be relative to
//! their rpath: an absolute name (rustc's default, the path it was built at) breaks every debug
//! build of a toolchain installed anywhere else.

#[cfg(target_os = "macos")]
#[test]
fn dylib_install_name_is_rpath_relative() {
    // target/<profile>/deps/<this test> → target/<profile>/libvelt_rt_shared.dylib
    let exe = std::env::current_exe().unwrap();
    let dylib = exe
        .parent()
        .and_then(|deps| deps.parent())
        .unwrap()
        .join("libvelt_rt_shared.dylib");
    assert!(dylib.is_file(), "{} not built", dylib.display());
    let Ok(out) = std::process::Command::new("otool")
        .arg("-D")
        .arg(&dylib)
        .output()
    else {
        eprintln!("skipped: no otool");
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.lines().nth(1).map(str::trim),
        Some("@rpath/libvelt_rt_shared.dylib"),
        "{text}"
    );
}
