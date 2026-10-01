//! The entry object of shared-runtime executables, for every target: `main` defined and global,
//! `velt_main` and `velt_rt_start` left to the linker (the first is in the program object, the
//! second in the shared runtime).

use object::{Object, ObjectSymbol};

use super::objects::{obj_name, TARGETS};
use crate::emit_entry_object;

#[test]
fn entry_object_for_every_target() {
    for (target, fmt, arch) in TARGETS {
        let bytes = emit_entry_object(target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let file = object::File::parse(&*bytes).unwrap_or_else(|e| panic!("{target}: {e}"));
        assert_eq!(
            (file.format(), file.architecture()),
            (fmt, arch),
            "{target}"
        );
        let find = |name: &str| {
            file.symbols()
                .find(|s| s.name() == Ok(obj_name(fmt, name).as_str()))
                .unwrap_or_else(|| panic!("{target}: no `{name}`"))
        };
        let main = find("main");
        assert!(main.is_definition() && main.is_global(), "{target}: main");
        for import in ["velt_main", "velt_rt_start"] {
            assert!(find(import).is_undefined(), "{target}: {import}");
        }
    }
}
