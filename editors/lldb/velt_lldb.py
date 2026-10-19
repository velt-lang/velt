"""LLDB support for Velt programs.

Loaded with `command script import <path>/velt_lldb.py`; the VS Code extension does this for
every debug session (`velt build --json` reports the path). Debug builds describe Velt values in
DWARF as C types named after their source types (docs/internals/design/debug-variables.md);
the formatters registered here, in the `velt` category, show them the way Velt prints them:

- `string`: its text (the three words of rt_abi.md "Strings": inline, static or heap);
- `T[]`: its elements, with `len=N` as the summary;
- unions and results (`... $tagged` structures): the active member or variant;
- `T | null` stored as `{ some, value }` (`... $option` structures): `null`, or the value.

Classes, enums without payloads, pointer options (`NULL`) and `shared<T>` need no formatter.
"""

import lldb

# Longest string text read from the program, in bytes.
STRING_LIMIT = 4096
# Most array elements shown.
ARRAY_LIMIT = 10000


def _formatted(value):
    """`value` as its formatters show it (its synthetic children, if it has a provider)."""
    get = getattr(value, "GetSyntheticValue", None)
    if get is not None:
        synthetic = get()
        return synthetic if synthetic.IsValid() else value
    # LLDB 18 and older have no `GetSyntheticValue`.
    value.SetPreferSyntheticValue(True)
    return value


def _readable(value, address, size):
    """Whether the program's memory at `address` (`size` bytes, at least one) can be read."""
    if size <= 0:
        return True
    error = lldb.SBError()
    value.GetProcess().ReadMemory(address + size - 1, 1, error)
    return error.Success()


def _decode(raw):
    """WTF-8 bytes as text: a lone surrogate shows as one U+FFFD."""
    try:
        text = raw.decode("utf-8", "surrogatepass")
    except UnicodeDecodeError:
        return raw.decode("utf-8", "replace")
    return "".join("\ufffd" if 0xD800 <= ord(c) <= 0xDFFF else c for c in text)


def _word(value, name):
    return value.GetChildMemberWithName(name).GetValueAsUnsigned(0)


def _quote(text):
    out = text.replace("\\", "\\\\").replace('"', '\\"')
    return '"' + out.replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t") + '"'


def string_text(value):
    """The text of a `string` value; `None` if it cannot be one (not yet initialized) or its
    bytes cannot be read."""
    value = value.GetNonSyntheticValue()
    w0, w1, w2 = (_word(value, n) for n in ("w0", "w1", "w2"))
    top = (w2 >> 56) & 0xFF
    if top >= 0x80:
        # Inline: the text is in the value's own bytes, its length in byte 23 (at most 23, or
        # 22 with the UTF-16 unit count in byte 22).
        n = top & 0x1F
        if n > 23 or (top & 0x40 and n > 22):
            return None
        raw = (w0.to_bytes(8, "little") + w1.to_bytes(8, "little") + w2.to_bytes(8, "little"))[:n]
        return _decode(raw)
    # Static, borrowed or heap: a pointer, then the UTF-16 unit count and the byte length in the
    # halves of word 1 (a string is shorter than 2 GiB, and has 1 to 3 bytes per unit).
    n, units = w1 & 0xFFFFFFFF, w1 >> 32
    if n > 0x7FFFFFFF or not units <= n <= 3 * units:
        return None
    if n == 0:
        return ""
    error = lldb.SBError()
    raw = value.GetProcess().ReadMemory(w0, min(n, STRING_LIMIT), error)
    if not error.Success():
        return None
    text = _decode(raw)
    return text + "…" if n > STRING_LIMIT else text


def string_summary(value, internal_dict):
    text = string_text(value)
    return "<invalid string>" if text is None else _quote(text)


class TextProvider:
    """No children for a `string`: its summary is its text, not its words."""

    def __init__(self, value, internal_dict):
        pass

    def update(self):
        return False

    def has_children(self):
        return False

    def num_children(self):
        return 0

    def get_child_index(self, name):
        return -1

    def get_child_at_index(self, index):
        return None


class ArrayProvider:
    """The elements of a `T[]` (`{ data: T*, len, cap }`)."""

    def __init__(self, value, internal_dict):
        self.value = value

    def update(self):
        self.data = self.value.GetChildMemberWithName("data")
        self.elem = self.data.GetType().GetPointeeType()
        self.size = self.elem.GetByteSize()
        self.len = min(array_len(self.value) or 0, ARRAY_LIMIT)
        return False

    def has_children(self):
        return True

    def num_children(self):
        return self.len

    def get_child_index(self, name):
        try:
            return int(name.lstrip("[").rstrip("]"))
        except ValueError:
            return -1

    def get_child_at_index(self, index):
        if index < 0 or index >= self.len:
            return None
        return self.data.CreateChildAtOffset("[%d]" % index, index * self.size, self.elem)


def array_len(value):
    """The length of a `T[]`, `None` if it cannot be an array (not yet initialized: a length
    above its capacity, or elements that cannot be read)."""
    raw = value.GetNonSyntheticValue()
    length, cap = _word(raw, "len"), _word(raw, "cap")
    data = raw.GetChildMemberWithName("data")
    size = data.GetType().GetPointeeType().GetByteSize()
    if length > cap or not _readable(raw, data.GetValueAsUnsigned(0), length * size):
        return None
    return length


def array_summary(value, internal_dict):
    length = array_len(value)
    return "<invalid array>" if length is None else "len=%d" % length


def _variant(value):
    """The active variant structure of a `$tagged` value, `None` for an unknown tag. A union's
    variant holds its member as `value`."""
    value = value.GetNonSyntheticValue()
    tag = _word(value, "tag")
    variants = value.GetChildAtIndex(1)
    if tag >= variants.GetNumChildren():
        return None
    return variants.GetChildAtIndex(tag)


class TaggedProvider:
    """The payload of the active variant of a union (its member's own children) or result."""

    def __init__(self, value, internal_dict):
        self.value = value
        self.variant = None

    def update(self):
        self.variant = _variant(self.value)
        if self.variant is not None:
            member = self.variant.GetChildMemberWithName("value")
            if self.variant.GetNumChildren() == 1 and member.IsValid():
                self.variant = _formatted(member)
        return False

    def has_children(self):
        return self.variant is not None and self.variant.GetNumChildren() > 0

    def num_children(self):
        return 0 if self.variant is None else self.variant.GetNumChildren()

    def get_child_index(self, name):
        return -1 if self.variant is None else self.variant.GetIndexOfChildWithName(name)

    def get_child_at_index(self, index):
        return None if self.variant is None else self.variant.GetChildAtIndex(index)


def tagged_summary(value, internal_dict):
    variant = _variant(value)
    if variant is None:
        return "<invalid tag %d>" % _word(value.GetNonSyntheticValue(), "tag")
    name = variant.GetName()
    # A union's variant holds its member as `value`: show that value; a result shows its variant
    # (`Ok`, `Err`) with the payload below.
    inner = variant.GetChildMemberWithName("value")
    if variant.GetNumChildren() == 1 and inner.IsValid():
        return inner.GetSummary() or inner.GetValue() or name
    return name


class OptionProvider:
    """The value of a present `T | null` stored as `{ some, value }`."""

    def __init__(self, value, internal_dict):
        self.value = value
        self.inner = None

    def update(self):
        raw = self.value.GetNonSyntheticValue()
        some = raw.GetChildMemberWithName("some").GetValueAsUnsigned(0)
        self.inner = _formatted(raw.GetChildMemberWithName("value")) if some else None
        return False

    def has_children(self):
        return self.inner is not None and self.inner.MightHaveChildren()

    def num_children(self):
        return 0 if self.inner is None else self.inner.GetNumChildren()

    def get_child_index(self, name):
        return -1 if self.inner is None else self.inner.GetIndexOfChildWithName(name)

    def get_child_at_index(self, index):
        return None if self.inner is None else self.inner.GetChildAtIndex(index)


def option_summary(value, internal_dict):
    raw = value.GetNonSyntheticValue()
    if not raw.GetChildMemberWithName("some").GetValueAsUnsigned(0):
        return "null"
    inner = raw.GetChildMemberWithName("value")
    return inner.GetSummary() or inner.GetValue() or ""


def __lldb_init_module(debugger, internal_dict):
    module = __name__
    commands = [
        'type summary add -w velt -F %s.string_summary "string"' % module,
        'type synthetic add -w velt -l %s.TextProvider "string"' % module,
        'type summary add -w velt -e -x -F %s.array_summary "\\[\\]$"' % module,
        'type synthetic add -w velt -x -l %s.ArrayProvider "\\[\\]$"' % module,
        'type summary add -w velt -e -x -F %s.tagged_summary " \\$tagged$"' % module,
        'type synthetic add -w velt -x -l %s.TaggedProvider " \\$tagged$"' % module,
        'type summary add -w velt -e -x -F %s.option_summary " \\$option$"' % module,
        'type synthetic add -w velt -x -l %s.OptionProvider " \\$option$"' % module,
        "type category enable velt",
    ]
    for command in commands:
        debugger.HandleCommand(command)
