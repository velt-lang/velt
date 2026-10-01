//! `LowerOptions::native_inits`: `velt_main` initializes each package's native library, in
//! order, before the user `main` runs (native_abi.md "Start-up").

use super::builder::*;
use crate::vir::{Callee, Terminator};
use crate::{LowerOptions, NativeInit};

fn calls_in_main(p: &crate::vir::Program) -> Vec<String> {
    let main = p.funcs.iter().find(|f| f.symbol == "velt_main").unwrap();
    let mut out = vec![];
    for b in &main.blocks {
        if let Terminator::Call { callee, .. } = &b.term {
            out.push(match callee {
                Callee::Extern(id) => p.externs[id.0 as usize].symbol.clone(),
                Callee::Func(id) => p.funcs[id.0 as usize].symbol.clone(),
                Callee::Ptr { .. } => "<ptr>".into(),
            });
        }
    }
    out
}

#[test]
fn velt_main_initializes_native_libraries_first() {
    let mut pb = PB::new();
    let t = pb.t;
    let f = FB::new("main", t.unit);
    pb.add_main(f.build(vec![]));
    let p = pb.finish();
    let inits = [
        NativeInit {
            package: "db".into(),
            symbol: "velt_native_init_db".into(),
        },
        NativeInit {
            package: "img-codec".into(),
            symbol: "velt_native_init_img_codec".into(),
        },
    ];
    let opts = LowerOptions {
        native_inits: &inits,
        ..Default::default()
    };
    let v = crate::lower_with(&p, &opts);
    crate::verify(&v).unwrap_or_else(|e| panic!("{}\n{v}", e.join("\n")));
    let calls = calls_in_main(&v);
    assert_eq!(
        &calls[..6],
        [
            "velt_rt_native_api",
            "velt_native_init_db",
            "velt_rt_native_check",
            "velt_rt_native_api",
            "velt_native_init_img_codec",
            "velt_rt_native_check",
        ]
    );
    assert!(calls[6..].iter().any(|c| c.contains("main")), "{calls:?}");

    // Without native libraries nothing changes.
    let plain = crate::lower(&p);
    assert!(!calls_in_main(&plain).iter().any(|c| c.contains("native")));
}
