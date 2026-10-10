"""LLDB support for Velt programs.

Loaded with `command script import <path>/velt_lldb.py`; the VS Code extension does this for
every debug session (`velt build --json` reports the path). Velt values are described in DWARF
as C structs; the formatters registered here will show them the way Velt prints them (strings,
arrays, optionals, enums, closures). This first version registers nothing yet.
"""


def __lldb_init_module(debugger, internal_dict):
    pass
