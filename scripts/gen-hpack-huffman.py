#!/usr/bin/env python3
"""Generates std/net/http/h2/huffman_table.vlt: the HPACK Huffman code (RFC 7541
Appendix B) as static data for the HTTP/2 server's encoder and decoder.

- `HUFFMAN_CODES` / `HUFFMAN_LENGTHS`: each symbol's code (right-aligned) and its length in bits,
  257 symbols (256 is EOS).
- `HUFFMAN_FAST`: for each 8-bit prefix, the symbol of a code of 8 bits or fewer it begins with
  (symbol | length << 9), or 0: most characters of a header take one lookup.
- `HUFFMAN_SYMBOLS`, `HUFFMAN_FIRST`, `HUFFMAN_OFFSET`, `HUFFMAN_COUNT`: canonical decoding of the
  longer codes (the code is canonical: the codes of one length are consecutive).

The arrays are written one number per line, as `velt fmt` lays them out.

Run: python3 scripts/gen-hpack-huffman.py > std/net/http/h2/huffman_table.vlt
"""

# (code, length) of symbols 0..256, from RFC 7541 Appendix B.
CODES = [
    (0x1ff8, 13), (0x7fffd8, 23), (0xfffffe2, 28), (0xfffffe3, 28), (0xfffffe4, 28), (0xfffffe5, 28),
    (0xfffffe6, 28), (0xfffffe7, 28), (0xfffffe8, 28), (0xffffea, 24), (0x3ffffffc, 30), (0xfffffe9, 28),
    (0xfffffea, 28), (0x3ffffffd, 30), (0xfffffeb, 28), (0xfffffec, 28), (0xfffffed, 28), (0xfffffee, 28),
    (0xfffffef, 28), (0xffffff0, 28), (0xffffff1, 28), (0xffffff2, 28), (0x3ffffffe, 30), (0xffffff3, 28),
    (0xffffff4, 28), (0xffffff5, 28), (0xffffff6, 28), (0xffffff7, 28), (0xffffff8, 28), (0xffffff9, 28),
    (0xffffffa, 28), (0xffffffb, 28), (0x14, 6), (0x3f8, 10), (0x3f9, 10), (0xffa, 12),
    (0x1ff9, 13), (0x15, 6), (0xf8, 8), (0x7fa, 11), (0x3fa, 10), (0x3fb, 10),
    (0xf9, 8), (0x7fb, 11), (0xfa, 8), (0x16, 6), (0x17, 6), (0x18, 6),
    (0x0, 5), (0x1, 5), (0x2, 5), (0x19, 6), (0x1a, 6), (0x1b, 6),
    (0x1c, 6), (0x1d, 6), (0x1e, 6), (0x1f, 6), (0x5c, 7), (0xfb, 8),
    (0x7ffc, 15), (0x20, 6), (0xffb, 12), (0x3fc, 10), (0x1ffa, 13), (0x21, 6),
    (0x5d, 7), (0x5e, 7), (0x5f, 7), (0x60, 7), (0x61, 7), (0x62, 7),
    (0x63, 7), (0x64, 7), (0x65, 7), (0x66, 7), (0x67, 7), (0x68, 7),
    (0x69, 7), (0x6a, 7), (0x6b, 7), (0x6c, 7), (0x6d, 7), (0x6e, 7),
    (0x6f, 7), (0x70, 7), (0x71, 7), (0x72, 7), (0xfc, 8), (0x73, 7),
    (0xfd, 8), (0x1ffb, 13), (0x7fff0, 19), (0x1ffc, 13), (0x3ffc, 14), (0x22, 6),
    (0x7ffd, 15), (0x3, 5), (0x23, 6), (0x4, 5), (0x24, 6), (0x5, 5),
    (0x25, 6), (0x26, 6), (0x27, 6), (0x6, 5), (0x74, 7), (0x75, 7),
    (0x28, 6), (0x29, 6), (0x2a, 6), (0x7, 5), (0x2b, 6), (0x76, 7),
    (0x2c, 6), (0x8, 5), (0x9, 5), (0x2d, 6), (0x77, 7), (0x78, 7),
    (0x79, 7), (0x7a, 7), (0x7b, 7), (0x7ffe, 15), (0x7fc, 11), (0x3ffd, 14),
    (0x1ffd, 13), (0xffffffc, 28), (0xfffe6, 20), (0x3fffd2, 22), (0xfffe7, 20), (0xfffe8, 20),
    (0x3fffd3, 22), (0x3fffd4, 22), (0x3fffd5, 22), (0x7fffd9, 23), (0x3fffd6, 22), (0x7fffda, 23),
    (0x7fffdb, 23), (0x7fffdc, 23), (0x7fffdd, 23), (0x7fffde, 23), (0xffffeb, 24), (0x7fffdf, 23),
    (0xffffec, 24), (0xffffed, 24), (0x3fffd7, 22), (0x7fffe0, 23), (0xffffee, 24), (0x7fffe1, 23),
    (0x7fffe2, 23), (0x7fffe3, 23), (0x7fffe4, 23), (0x1fffdc, 21), (0x3fffd8, 22), (0x7fffe5, 23),
    (0x3fffd9, 22), (0x7fffe6, 23), (0x7fffe7, 23), (0xffffef, 24), (0x3fffda, 22), (0x1fffdd, 21),
    (0xfffe9, 20), (0x3fffdb, 22), (0x3fffdc, 22), (0x7fffe8, 23), (0x7fffe9, 23), (0x1fffde, 21),
    (0x7fffea, 23), (0x3fffdd, 22), (0x3fffde, 22), (0xfffff0, 24), (0x1fffdf, 21), (0x3fffdf, 22),
    (0x7fffeb, 23), (0x7fffec, 23), (0x1fffe0, 21), (0x1fffe1, 21), (0x3fffe0, 22), (0x1fffe2, 21),
    (0x7fffed, 23), (0x3fffe1, 22), (0x7fffee, 23), (0x7fffef, 23), (0xfffea, 20), (0x3fffe2, 22),
    (0x3fffe3, 22), (0x3fffe4, 22), (0x7ffff0, 23), (0x3fffe5, 22), (0x3fffe6, 22), (0x7ffff1, 23),
    (0x3ffffe0, 26), (0x3ffffe1, 26), (0xfffeb, 20), (0x7fff1, 19), (0x3fffe7, 22), (0x7ffff2, 23),
    (0x3fffe8, 22), (0x1ffffec, 25), (0x3ffffe2, 26), (0x3ffffe3, 26), (0x3ffffe4, 26), (0x7ffffde, 27),
    (0x7ffffdf, 27), (0x3ffffe5, 26), (0xfffff1, 24), (0x1ffffed, 25), (0x7fff2, 19), (0x1fffe3, 21),
    (0x3ffffe6, 26), (0x7ffffe0, 27), (0x7ffffe1, 27), (0x3ffffe7, 26), (0x7ffffe2, 27), (0xfffff2, 24),
    (0x1fffe4, 21), (0x1fffe5, 21), (0x3ffffe8, 26), (0x3ffffe9, 26), (0xffffffd, 28), (0x7ffffe3, 27),
    (0x7ffffe4, 27), (0x7ffffe5, 27), (0xfffec, 20), (0xfffff3, 24), (0xfffed, 20), (0x1fffe6, 21),
    (0x3fffe9, 22), (0x1fffe7, 21), (0x1fffe8, 21), (0x7ffff3, 23), (0x3fffea, 22), (0x3fffeb, 22),
    (0x1ffffee, 25), (0x1ffffef, 25), (0xfffff4, 24), (0xfffff5, 24), (0x3ffffea, 26), (0x7ffff4, 23),
    (0x3ffffeb, 26), (0x7ffffe6, 27), (0x3ffffec, 26), (0x3ffffed, 26), (0x7ffffe7, 27), (0x7ffffe8, 27),
    (0x7ffffe9, 27), (0x7ffffea, 27), (0x7ffffeb, 27), (0xffffffe, 28), (0x7ffffec, 27), (0x7ffffed, 27),
    (0x7ffffee, 27), (0x7ffffef, 27), (0x7fffff0, 27), (0x3ffffee, 26), (0x3fffffff, 30),]

def canonical():
    """The tables of canonical decoding: symbols ordered by (length, symbol), and for each length
    the first code of that length and the index of its first symbol in that order."""
    order = sorted(range(len(CODES)), key=lambda s: (CODES[s][1], s))
    first = [0] * 31
    offset = [0] * 31
    count = [0] * 31
    for s in order:
        count[CODES[s][1]] += 1
    k = 0
    for length in range(1, 31):
        offset[length] = k
        syms = [s for s in order if CODES[s][1] == length]
        first[length] = CODES[syms[0]][0] if syms else 0
        # Canonical: the codes of one length are consecutive, in symbol order.
        for i, sym in enumerate(syms):
            assert CODES[sym][0] == first[length] + i
        k += len(syms)
    return order, first, offset, count


def fast():
    """For each 8-bit prefix, the symbol whose code (8 bits or fewer) it begins with, as
    symbol | length << 9, or 0."""
    table = [0] * 256
    for sym, (code, length) in enumerate(CODES):
        if length <= 8:
            base = code << (8 - length)
            for low in range(1 << (8 - length)):
                table[base | low] = sym | length << 9
    return table


def numbers(values, per_line):
    return "\n".join(f"  {v}," for v in values)


def main():
    print("// velt/net/http/h2 (internal): the HPACK Huffman code (RFC 7541 Appendix B) as static data,")
    print("// for h2/huffman.vlt.")
    print("// generated by scripts/gen-hpack-huffman.py: do not edit.")
    print()
    print("/// Each symbol's code, right-aligned (symbol 256 is EOS).")
    print("export const HUFFMAN_CODES: u32[] = [")
    print(numbers([c for c, _ in CODES], 10))
    print("];")
    print()
    print("/// Each symbol's code length in bits.")
    print("export const HUFFMAN_LENGTHS: u8[] = [")
    print(numbers([l for _, l in CODES], 24))
    print("];")
    print()
    order, first, offset, count = canonical()
    print("/// For each 8-bit prefix: symbol | length << 9 of the code of 8 bits or fewer it begins")
    print("/// with, or 0.")
    print("export const HUFFMAN_FAST: u16[] = [")
    print(numbers(fast(), 1))
    print("];")
    print()
    print("/// The symbols ordered by code length, then symbol (canonical order).")
    print("export const HUFFMAN_SYMBOLS: u16[] = [")
    print(numbers(order, 1))
    print("];")
    print()
    print("/// For each code length: its first code.")
    print("export const HUFFMAN_FIRST: u32[] = [")
    print(numbers(first, 1))
    print("];")
    print()
    print("/// For each code length: where its symbols start in `HUFFMAN_SYMBOLS`.")
    print("export const HUFFMAN_OFFSET: u16[] = [")
    print(numbers(offset, 1))
    print("];")
    print()
    print("/// For each code length: how many codes have it.")
    print("export const HUFFMAN_COUNT: u16[] = [")
    print(numbers(count, 1))
    print("];")

main()
