"""In branches/units-u1a: moves the unit-encoding block at the end of each given file of
crates/velt_sema/src into a child module `codec` (the block and what follows it)."""
import pathlib, re, sys

W = pathlib.Path("/Users/andii/dev/velt/branches/units-u1a/crates/velt_sema/src")
for rel in sys.argv[1:]:
    p = W / rel
    lines = p.read_text().split("\n")
    at = next(i for i, l in enumerate(lines) if l.startswith("// The encoding of a finished unit"))
    block = "\n".join(lines[at:]).rstrip("\n") + "\n"
    head = lines[:at]
    while head and head[-1] == "":
        head.pop()
    child = p.with_name("codec.rs") if p.name == "mod.rs" else p.with_suffix("") / "codec.rs"
    child.parent.mkdir(exist_ok=True)
    block = block.replace("// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.\n", "", 1)
    block = block.replace("// The encoding of a finished unit (`units::codec`): every variant, so a new one fails to build.\n", "", 1)
    child.write_text("//! The encoding of this module's types in a finished unit (`units::codec`): every field and\n"
                     "//! variant, so a new one fails to build.\n\nuse super::*;\n\n" + block)
    # `mod codec;` before the first `use` line of the parent.
    first_use = next(i for i, l in enumerate(head) if re.match(r"^(pub(\(crate\))? )?use ", l))
    head.insert(first_use, "")
    head.insert(first_use, "mod codec;")
    p.write_text("\n".join(head) + "\n")
    print(rel, "->", child.relative_to(W), len(head) + 1, "lines left")
