#!/usr/bin/env python3
# Generates the SIMD API, `velt/simd` (std/simd/index.vlt and std/simd/*.vlt): the same methods for ten vector types
# and four mask types, which Velt cannot write once (no generics over lane types). To change
# the API, change this script and run it from the repository root, then `velt fmt` the files:
#
#   python3 scripts/gen-simd-api.py && velt fmt std/simd/index.vlt std/simd/*.vlt
#
# The operations themselves are in crates/velt_vir/src/simd.rs (one table for both backends);
# a test there checks every declaration written here against it.
import os
STD = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "std")
OUT = os.path.join(STD, "simd")
TYPES = [("i8",16),("u8",16),("i16",8),("u16",8),("i32",4),("u32",4),("i64",2),("u64",2),("f32",4),("f64",2)]
def name(l,n): return f"{l}x{n}"
def bits(l): return int(l[1:])
def mask(l,n): return f"mask{bits(l)}x{n}"
def isf(l): return l[0]=="f"
def signed(l): return l[0]=="i"
def supported(l,op):
    f=isf(l); b=bits(l)
    return {"div":f,"sqrt":f,"mul":f or b!=8,"neg":f or signed(l),"abs":f or (signed(l) and b!=64),
            "min":f or b!=64,"max":f or b!=64,"add_sat":(not f) and b<=16,"sub_sat":(not f) and b<=16,
            "and":not f,"or":not f,"xor":not f,"not":not f,"shl":not f,"shr":not f}.get(op,True)
def camel(op):
    p=op.split("_"); return p[0]+"".join(x.capitalize() for x in p[1:])
def cast_targets(l,n):
    same=[(l2,n2) for (l2,n2) in TYPES if n2==n and l2!=l]
    hub=[("u8",16)] if (l,n)!=("u8",16) else [t for t in TYPES if t!=("u8",16)]
    out=[]
    for t in same+hub:
        if t not in out and t!=(l,n): out.append(t)
    return out
def tname(l,n): return name(l,n)[0].upper()+name(l,n)[1:]

BIN_DOC = {
 "add":"Lane-wise sum{w}.","sub":"Lane-wise difference{w}.","mul":"Lane-wise product{w}.",
 "div":"Lane-wise quotient.","min":"The smaller of each pair of lanes{nan}.","max":"The larger of each pair of lanes{nan}.",
 "add_sat":"Lane-wise sum, clamped to the lane type's range instead of wrapping.",
 "sub_sat":"Lane-wise difference, clamped to the lane type's range instead of wrapping.",
 "and":"Bitwise AND.","or":"Bitwise OR.","xor":"Bitwise XOR.",
}
CMP_DOC = {"eq":"equals","ne":"differs from","lt":"is less than","le":"is at most","gt":"is greater than","ge":"is at least"}

def vector(l,n):
    T=name(l,n); M=mask(l,n); f=isf(l); P=f"velt_simd_{T}_"
    d=[]; m=[]
    def decl(op,params,ret=None):
        d.append(f"declare function {P}{op}({params}){': '+ret if ret else ''};")
    decl("fits",f"xs: {l}[], at: u64","bool"); decl("splat",f"x: {l}",T); decl("index","",T); decl("load",f"from: {l}[], at: u64",T); decl("store",f"v: {T}, to: {l}[], at: u64")
    decl("load_partial",f"from: {l}[], at: u64, count: u64",T); decl("store_partial",f"v: {T}, to: {l}[], at: u64, count: u64")
    decl("lane",f"v: {T}, index: u64",l); decl("with_lane",f"v: {T}, index: u64, x: {l}",T)
    decl("select",f"m: {M}, a: {T}, b: {T}",T)
    m.append(f"""  /** A vector with every lane `x`. */
  static splat(x: {l}): {T} {{
    return {P}splat(x);
  }}

  /**
   * The {n} elements of `from` starting at `at`. Panics with a RangeError unless all {n} are
   * inside the array.
   */
  static load(from: {l}[], at: usize = 0): {T} {{
    if (!{T}.fits(from, at)) {{
      panic("RangeError: {T}.load: range is out of bounds");
    }}
    return {P}load(from, at as u64);
  }}

  /**
   * Whether the {n} elements of `xs` starting at `at` are inside the array: what `load` and
   * `store` check. As the condition of the loop that calls them (`while ({T}.fits(xs, i))`),
   * it leaves them nothing to check.
   */
  static fits(xs: {l}[], at: usize = 0): bool {{
    return {P}fits(xs, at as u64);
  }}

  /** The vector whose lanes are their own indices: 0, 1, 2, … */
  static index(): {T} {{
    return {P}index();
  }}

  /**
   * The elements of `from` starting at `at` that exist, up to {n}, in the low lanes; the other
   * lanes are 0. For the last elements of an array, after the steps that take {n} at a time.
   * `at` may be the length (no element left); past it, panics with a RangeError.
   */
  static loadPartial(from: {l}[], at: usize = 0): {T} {{
    if ({T}.fits(from, at)) {{
      return {P}load(from, at as u64);
    }}
    if (at > from.length) {{
      panic("RangeError: {T}.loadPartial: position is out of range");
    }}
    return {P}load_partial(from, at as u64, (from.length - at) as u64);
  }}

  /** Lane by lane, `a` where the mask is set and `b` elsewhere. */
  static select(m: {M}, a: {T}, b: {T}): {T} {{
    return {P}select(m, a, b);
  }}

  /**
   * Writes the {n} lanes to `to` starting at `at`. Panics with a RangeError unless all {n}
   * elements are inside the array (it does not grow).
   */
  store(to: {l}[], at: usize = 0) {{
    if (!{T}.fits(to, at)) {{
      panic("RangeError: {T}.store: range is out of bounds");
    }}
    {P}store(this, to, at as u64);
  }}

  /**
   * Writes the low lanes to the elements of `to` starting at `at` that exist, up to {n} (the
   * array does not grow): what `loadPartial` read, back. `at` may be the length (nothing is
   * written); past it, panics with a RangeError.
   */
  storePartial(to: {l}[], at: usize = 0) {{
    if ({T}.fits(to, at)) {{
      {P}store(this, to, at as u64);
      return;
    }}
    if (at > to.length) {{
      panic("RangeError: {T}.storePartial: position is out of range");
    }}
    {P}store_partial(this, to, at as u64, (to.length - at) as u64);
  }}

  /** Lane `index` (0 to {n-1}; panics with a RangeError otherwise). */
  lane(index: usize): {l} {{
    if (index >= {n}) {{
      panic("RangeError: {T}.lane: index is out of range");
    }}
    return {P}lane(this, index as u64);
  }}

  /** This vector with lane `index` (0 to {n-1}; panics with a RangeError otherwise) set to `x`. */
  withLane(index: usize, x: {l}): {T} {{
    if (index >= {n}) {{
      panic("RangeError: {T}.withLane: index is out of range");
    }}
    return {P}with_lane(this, index as u64, x);
  }}
""")
    w="" if f else ", wrapping on overflow"
    nan=" (as `Math.min` and `Math.max`: NaN if either is)" if f else ""
    for op in ["add","sub","mul","div","min","max","add_sat","sub_sat","and","or","xor"]:
        if not supported(l,op): continue
        decl(op,f"a: {T}, b: {T}",T)
        m.append(f"""  /** {BIN_DOC[op].format(w=w,nan=nan)} */
  {camel(op)}(other: {T}): {T} {{
    return {P}{op}(this, other);
  }}
""")
    for op,doc in [("neg","Each lane negated" + ("" if f else " (wrapping)") + "."),("abs","The absolute value of each lane" + ("" if f else " (the smallest value stays itself)") + "."),("sqrt","The square root of each lane."),("not","Every bit flipped.")]:
        if not supported(l,op): continue
        decl(op,f"v: {T}",T)
        m.append(f"""  /** {doc} */
  {op}(): {T} {{
    return {P}{op}(this);
  }}
""")
    if not f:
        b=bits(l)
        for op,doc in [("shl",f"Each lane shifted left by `count` bits (taken modulo {b})."),
                       ("shr",f"Each lane shifted right by `count` bits (taken modulo {b}), {'keeping the sign' if signed(l) else 'filling with zeros'}.")]:
            decl(op,f"v: {T}, count: u32",T)
            m.append(f"""  /** {doc} */
  {op}(count: u32): {T} {{
    return {P}{op}(this, count);
  }}
""")
    for op in ["eq","ne","lt","le","gt","ge"]:
        decl(op,f"a: {T}, b: {T}",M)
        extra = " (a NaN lane equals nothing)" if f and op=="eq" else (" (a NaN lane differs from everything)" if f and op=="ne" else "")
        m.append(f"""  /** The lanes where this vector {CMP_DOC[op]} `other`{extra}. */
  {op}(other: {T}): {M} {{
    return {P}{op}(this, other);
  }}
""")
    if bits(l)>=32:
        terms=" + ".join(f"{P}lane(this, {i})" for i in range(n))
        m.append(f"""  /** The sum of the lanes{'' if f else ', wrapping on overflow'}{' (added in lane order)' if f and n>2 else ''}. */
  sum(): {l} {{
    return {terms};
  }}
""")
    if not f and bits(l)>=16:
        decl("reverse_bytes",f"v: {T}",T)
        m.append(f"""  /**
   * The bytes of each lane in reverse order: big-endian {bits(l)}-bit words read with `load`
   * become their values, and back (`rev{bits(l)}`, `pshufb`).
   */
  reverseBytes(): {T} {{
    return {P}reverse_bytes(this);
  }}
""")
    decl("zip_low",f"a: {T}, b: {T}",T); decl("zip_high",f"a: {T}, b: {T}",T)
    m.append(f"""  /** The lanes of the low halves of this vector and `other` in turn: a0, b0, a1, b1, … */
  zipLow(other: {T}): {T} {{
    return {P}zip_low(this, other);
  }}

  /** The lanes of the high halves of this vector and `other` in turn. */
  zipHigh(other: {T}): {T} {{
    return {P}zip_high(this, other);
  }}
""")
    if not f and bits(l)<=32:
        W=name(l[0]+str(bits(l)*2),n//2)
        decl("widen_low",f"v: {T}",W); decl("widen_high",f"v: {T}",W)
        m.append(f"""  /** The low {n//2} lanes, each as a `{l[0]}{bits(l)*2}`. */
  widenLow(): {W} {{
    return {P}widen_low(this);
  }}

  /** The high {n//2} lanes, each as a `{l[0]}{bits(l)*2}`. */
  widenHigh(): {W} {{
    return {P}widen_high(this);
  }}
""")
    if not f and bits(l) in (16,32):
        N=name(l[0]+str(bits(l)//2),n*2)
        decl("narrow",f"a: {T}, b: {T}",N)
        m.append(f"""  /**
   * The lanes of this vector, then of `other`, each as a `{l[0]}{bits(l)//2}`, clamped to its range
   * instead of wrapping.
   */
  narrow(other: {T}): {N} {{
    return {P}narrow(this, other);
  }}
""")
    if l in ("i32","u32"):
        decl("to_f32x4",f"v: {T}","f32x4")
        m.append(f"""  /** Each lane as an `f32`, rounded to the nearest one. */
  toF32x4(): f32x4 {{
    return {P}to_f32x4(this);
  }}
""")
    if l=="f32":
        for d2 in ("i32","u32"):
            D=name(d2,4)
            decl(f"to_{D}",f"v: {T}",D)
            m.append(f"""  /** Each lane as {'an' if d2=='i32' else 'a'} `{d2}`: truncated toward zero, clamped to its range, NaN as 0. */
  to{tname(d2,4)}(): {D} {{
    return {P}to_{D}(this);
  }}
""")
    for (l2,n2) in cast_targets(l,n):
        T2=name(l2,n2)
        decl(f"as_{T2}",f"v: {T}",T2)
        m.append(f"""  /** The same 128 bits as a `{T2}` (no conversion of values). */
  as{tname(l2,n2)}(): {T2} {{
    return {P}as_{T2}(this);
  }}
""")
    return "\n".join(d)+f"\n\nextend {T} {{\n"+"\n".join(m)+"}\n"

def maskty(b,n):
    M=f"mask{b}x{n}"; P=f"velt_simd_{M}_"; U=f"u{b}x{n}"; UP=f"velt_simd_{U}_"
    return f"""declare function {UP}index(): {U};
declare function {UP}splat(x: u{b}): {U};
declare function {UP}lt(a: {U}, b: {U}): {M};
declare function {P}any(m: {M}): bool;
declare function {P}all(m: {M}): bool;
declare function {P}bitmask(m: {M}): u64;
declare function {P}first(m: {M}): u64;
declare function {P}count(m: {M}): u64;
declare function {P}and(a: {M}, b: {M}): {M};
declare function {P}or(a: {M}, b: {M}): {M};
declare function {P}xor(a: {M}, b: {M}): {M};
declare function {P}not(m: {M}): {M};

extend {M} {{
  /**
   * The mask with its first `count` lanes set (all {n} from {n} up): the lanes `loadPartial`
   * filled when `count` elements were left.
   */
  static prefix(count: usize): {M} {{
    const lanes = count < {n} ? count : {n};
    return {UP}lt({UP}index(), {UP}splat(lanes as u{b}));
  }}

  /** Whether some lane is set. */
  any(): bool {{
    return {P}any(this);
  }}

  /** Whether every lane is set. */
  all(): bool {{
    return {P}all(this);
  }}

  /** The lanes as bits: bit `i` is lane `i`. */
  bitmask(): u64 {{
    return {P}bitmask(this);
  }}

  /** The index of the first set lane, or {n} when none is. */
  first(): usize {{
    return {P}first(this) as usize;
  }}

  /** The number of set lanes. */
  count(): usize {{
    return {P}count(this) as usize;
  }}

  /** The lanes set in both masks. */
  and(other: {M}): {M} {{
    return {P}and(this, other);
  }}

  /** The lanes set in either mask. */
  or(other: {M}): {M} {{
    return {P}or(this, other);
  }}

  /** The lanes set in exactly one of the masks. */
  xor(other: {M}): {M} {{
    return {P}xor(this, other);
  }}

  /** The lanes that are not set. */
  not(): {M} {{
    return {P}not(this);
  }}
}}
"""

HEAD = """// velt/simd/{part}: 128-bit SIMD, {what}. Programs import the types from `velt/simd`.
//
// A vector is a value like a number: sixteen `u8` lanes in a `u8x16`, four `f32` in an
// `f32x4`. Every method is one instruction, or a few, on x86-64 (SSE2), arm64 (NEON) and
// WebAssembly (`simd128`), the same in debug and release builds; the `velt_simd_*` functions
// below have no runtime implementation, the compiler emits them inline (rt_abi.md "SIMD").
// Comparisons give a mask (`mask8x16`), which `any()`, `first()`, `bitmask()` and `select`
// consume. In std the type names are built in; each `export type` below gives a module that
// imports one the built-in type.
//
// Generated by scripts/gen-simd-api.py: change the script, not this file.
"""
def exports(names):
    return "".join(f"export type {t} = {t};\n" for t in names)
files = {
 "bytes": ("8- and 16-bit lanes", [t for t in TYPES if bits(t[0])<=16]),
 "words": ("32- and 64-bit integer lanes", [t for t in TYPES if bits(t[0])>=32 and not isf(t[0])]),
 "floats": ("float lanes", [t for t in TYPES if isf(t[0])]),
}
for part,(what,ts) in files.items():
    head = HEAD.format(part=part, what=what)+"\n"+exports(name(l,n) for l,n in ts)+"\n"
    open(f"{OUT}/{part}.vlt","w",encoding="utf-8",newline="\n").write(head+"\n".join(vector(l,n) for l,n in ts))
MASKS = [f"mask{b}x{128//b}" for b in (8,16,32,64)]
head = HEAD.format(part="masks", what="masks")+"\n"+exports(MASKS)+"\n"
open(f"{OUT}/masks.vlt","w",encoding="utf-8",newline="\n").write(head+"\n".join(maskty(b,128//b) for b in (8,16,32,64)))
reexports = "".join(
    f"export {{ {', '.join(name(l,n) for l,n in ts)} }} from \"velt/simd/{part}\";\n"
    for part,(_,ts) in files.items())
open(f"{STD}/simd.vlt","w",encoding="utf-8",newline="\n").write(f"""// velt/simd: the 128-bit SIMD vector types and their masks (docs/std/simd.md).
//
//   import {{ u8x16, mask8x16 }} from "velt/simd";
//
// Importing any of them loads the whole API: the methods are `extend` blocks, which apply
// wherever their module is loaded, and the types convert to each other.
//
// Generated by scripts/gen-simd-api.py: change the script, not this file.

{reexports}export {{ {", ".join(MASKS)} }} from "velt/simd/masks";
// The CPU's SHA instructions on `u32x4` (hand-written, not generated).
import "velt/simd/sha";
// Carry-less multiplication on `u64x2` (hand-written, not generated).
import "velt/simd/clmul";
""")
