#!/usr/bin/env python3
# Generates tests/golden/std/simd_ints.vlt: every integer operation of every vector type
# against lanes computed here with Python integers, independent of both backends. Run it from
# the repository root, format the file and record what it prints:
#
#   python3 scripts/gen-simd-golden.py && velt fmt tests/golden/std/simd_ints.vlt
#   velt run tests/golden/std/simd_ints.vlt > tests/golden/std/simd_ints.out
import os, re
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tests", "golden", "std")
INTS = [("i8",16),("u8",16),("i16",8),("u16",8),("i32",4),("u32",4),("i64",2),("u64",2)]
def bits(l): return int(l[1:])
def signed(l): return l[0]=="i"
def wrap(l,v):
    b=bits(l); v&=(1<<b)-1
    return v-(1<<b) if signed(l) and v>>(b-1) else v
def lo(l): return -(1<<(bits(l)-1)) if signed(l) else 0
def hi(l): return (1<<(bits(l)-1))-1 if signed(l) else (1<<bits(l))-1
def sat(l,v): return max(lo(l),min(hi(l),v))
def lit(l,v):
    if l=="i64" and v==lo(l): return "-9223372036854775807 - 1"
    return str(v)
def arr(l,vs): return "["+", ".join(lit(l,v) for v in vs)+"]"
def tname(T): return T[0].upper()+T[1:]
def cast_targets(l,n):
    ALL=INTS+[("f32",4),("f64",2)]
    same=[(a,b) for (a,b) in ALL if b==n and a!=l]
    hub=[("u8",16)] if (l,n)!=("u8",16) else [t for t in ALL if t!=("u8",16)]
    out=[]
    for t in same+hub:
        if t not in out: out.append(t)
    return out

SIMD_NAMES = ["i8x16","u8x16","i16x8","u16x8","i32x4","u32x4","i64x2","u64x2","f32x4","f64x2",
              "mask8x16","mask16x8","mask32x4","mask64x2"]

def ints():
    o=["// Every integer SIMD operation against lanes computed outside the compiler (generated;",
       "// the expected values are Python integer arithmetic). Prints the number of checks per",
       "// type, and a line per lane that differs.",""]
    for l,n in INTS:
        T=f"{l}x{n}"; b=bits(l); M=f"mask{b}x{n}"
        pool_a=[0,1,hi(l),lo(l),hi(l)-1,5,100,lo(l)+1,3,77,hi(l)//2,lo(l)//2 if signed(l) else 200,9,64,2,hi(l)-3]
        pool_b=[1,hi(l),1,lo(l) if signed(l) else 1,2,5,27,hi(l),4,77,3,100,0,65,hi(l)-1,7]
        A=[wrap(l,v) for v in pool_a[:n]]; B=[wrap(l,v) for v in pool_b[:n]]
        o.append(f"function lanes{tname(T)}(what: string, got: {T}, want: {l}[]): i64 {{")
        o.append(f"  for (let i: usize = 0; i < {n}; i++) {{")
        o.append(f"    if (got.lane(i) != want[i]) {{")
        o.append(f"      console.log(\"FAIL\", what, \"lane\", i, got.lane(i), want[i]);")
        o.append("    }\n  }\n  return 1;\n}\n")
        o.append(f"function test{tname(T)}(): i64 {{")
        o.append(f"  const a: {l}[] = {arr(l,A)};")
        o.append(f"  const b: {l}[] = {arr(l,B)};")
        o.append(f"  const va = {T}.load(a);\n  const vb = {T}.load(b);\n  let n = 0;")
        def chk(what,expr,want): o.append(f"  n += lanes{tname(T)}(\"{T}.{what}\", {expr}, {arr(l,want)});")
        def bm(what,expr,flags):
            v=sum(1<<i for i,f in enumerate(flags) if f)
            o.append(f"  n += bitmask(\"{T}.{what}\", {expr}.bitmask(), {v});")
        chk("load","va",A)
        o.append(f"  n += flag(\"{T}.fits\", {T}.fits(a), true);")
        o.append(f"  n += flag(\"{T}.fits past the end\", {T}.fits(a, 1), false);")
        o.append(f"  n += flag(\"{T}.fits wrapping\", {T}.fits(a, 18446744073709551615), false);")
        chk("splat",f"{T}.splat({lit(l,A[1])})",[A[1]]*n)
        chk("add","va.add(vb)",[wrap(l,x+y) for x,y in zip(A,B)])
        chk("sub","va.sub(vb)",[wrap(l,x-y) for x,y in zip(A,B)])
        if b!=8: chk("mul","va.mul(vb)",[wrap(l,x*y) for x,y in zip(A,B)])
        if b!=64:
            chk("min","va.min(vb)",[min(x,y) for x,y in zip(A,B)])
            chk("max","va.max(vb)",[max(x,y) for x,y in zip(A,B)])
        if b<=16:
            chk("addSat","va.addSat(vb)",[sat(l,x+y) for x,y in zip(A,B)])
            chk("subSat","va.subSat(vb)",[sat(l,x-y) for x,y in zip(A,B)])
        if signed(l):
            chk("neg","va.neg()",[wrap(l,-x) for x in A])
            if b!=64: chk("abs","va.abs()",[wrap(l,abs(x)) for x in A])
        chk("and","va.and(vb)",[wrap(l,x&y) for x,y in zip(A,B)])
        chk("or","va.or(vb)",[wrap(l,x|y) for x,y in zip(A,B)])
        chk("xor","va.xor(vb)",[wrap(l,x^y) for x,y in zip(A,B)])
        chk("not","va.not()",[wrap(l,~x) for x in A])
        for c in (0,1,b-1,b+3):
            k=c%b
            chk(f"shl({c})",f"va.shl({c})",[wrap(l,x<<k) for x in A])
            chk(f"shr({c})",f"va.shr({c})",[wrap(l,x>>k) if signed(l) else (x&((1<<b)-1))>>k for x in A])
        import operator as op
        for name,f in [("eq",op.eq),("ne",op.ne),("lt",op.lt),("le",op.le),("gt",op.gt),("ge",op.ge)]:
            bm(name,f"va.{name}(vb)",[f(x,y) for x,y in zip(A,B)])
        lt=[x<y for x,y in zip(A,B)]
        chk("select",f"{T}.select(va.lt(vb), va, vb)",[x if c else y for c,x,y in zip(lt,A,B)])
        o.append(f"  n += count(\"{T}.lt count\", va.lt(vb).count(), {sum(lt)});")
        first=lt.index(True) if True in lt else n
        o.append(f"  n += count(\"{T}.lt first\", va.lt(vb).first(), {first});")
        eq=[x==y for x,y in zip(A,B)]
        o.append(f"  n += flag(\"{T}.eq any\", va.eq(vb).any(), {str(any(eq)).lower()});")
        o.append(f"  n += flag(\"{T}.eq all\", va.eq(vb).all(), {str(all(eq)).lower()});")
        o.append(f"  n += flag(\"{T}.eq self all\", va.eq(va).all(), true);")
        o.append(f"  n += flag(\"{T}.ne self any\", va.ne(va).any(), false);")
        bm("mask and","va.lt(vb).and(va.ne(vb))",[c and d for c,d in zip(lt,[not e for e in eq])])
        bm("mask or","va.lt(vb).or(va.eq(vb))",[c or d for c,d in zip(lt,eq)])
        bm("mask xor","va.le(vb).xor(va.eq(vb))",lt)
        bm("mask not","va.lt(vb).not()",[not c for c in lt])
        w=list(A); w[n-1]=B[0]
        chk("withLane",f"va.withLane({n-1}, {lit(l,B[0])})",w)
        if b>=32:
            o.append(f"  n += lanes{tname(T)}(\"{T}.sum\", {T}.splat(va.sum()), {arr(l,[wrap(l,sum(A))]*n)});")
        # store at an offset
        o.append(f"  const out: {l}[] = {arr(l,[A[0]]*(n+2))};")
        o.append(f"  vb.store(out, 2);")
        o.append(f"  n += lanes{tname(T)}(\"{T}.store\", {T}.load(out, 1), {arr(l,[A[0]]+B[:n-1])});")
        # reinterpretation: little-endian bytes, and back
        if (l,n)!=("u8",16):
            by=[]
            for x in A:
                u=x&((1<<b)-1)
                by+= [(u>>(8*k))&255 for k in range(b//8)]
            o.append(f"  n += lanesU8x16(\"{T}.asU8x16\", va.asU8x16(), {arr('u8',by)});")
            o.append(f"  n += lanes{tname(T)}(\"{T} round trip\", va.asU8x16().as{tname(T)}(), {arr(l,A)});")
        chk("index",f"{T}.index()",list(range(n)))
        src=[A[0]]*2+B  # the array `out` after the store above: A0, A0, then B's lanes
        for at in range(len(src) + 1):
            part=(src[at:at+n]+[0]*n)[:n]
            o.append(f"  n += lanes{tname(T)}(\"{T}.loadPartial({at})\", {T}.loadPartial(out, {at}), {arr(l,part)});")
        o.append(f"  const few: {l}[] = {arr(l,A[:n-1])};")
        o.append(f"  vb.storePartial(few, 1);")
        o.append(f"  vb.storePartial(few, {n-1});")
        o.append(f"  n += lanes{tname(T)}(\"{T}.storePartial\", {T}.loadPartial(few), {arr(l,([A[0]]+B+[0]*n)[:n-1]+[0])});")
        for k in (0, 1, n - 1, n, n + 5):
            o.append(f"  n += bitmask(\"{M}.prefix({k})\", {M}.prefix({k}).bitmask(), {(1<<min(k,n))-1});")
        tw=lambda L,N: tname(f"{L}x{N}")
        low=[v for k in range(n//2) for v in (A[k],B[k])]
        high=[v for k in range(n//2,n) for v in (A[k],B[k])]
        chk("zipLow","va.zipLow(vb)",low)
        chk("zipHigh","va.zipHigh(vb)",high)
        if b<=32:
            wl=l[0]+str(b*2)
            o.append(f"  n += lanes{tw(wl,n//2)}(\"{T}.widenLow\", va.widenLow(), {arr(wl,A[:n//2])});")
            o.append(f"  n += lanes{tw(wl,n//2)}(\"{T}.widenHigh\", va.widenHigh(), {arr(wl,A[n//2:])});")
        if b in (16,32):
            nl=l[0]+str(b//2)
            o.append(f"  n += lanes{tw(nl,n*2)}(\"{T}.narrow\", va.narrow(vb), {arr(nl,[sat(nl,x) for x in A+B])});")
            # The values next to each end of the narrower range, and the far ends.
            edge=[hi(nl),hi(nl)+1,lo(nl),lo(nl)-1 if signed(l) else hi(nl)-1,hi(l),lo(l),(hi(l)+1)//2,(hi(l)+1)//2+1][:n]
            edge=[wrap(l,v) for v in edge]
            o.append(f"  const edge: {l}[] = {arr(l,edge)};")
            o.append(f"  n += lanes{tw(nl,n*2)}(\"{T}.narrow edges\", {T}.load(edge).narrow({T}.load(edge)), {arr(nl,[sat(nl,x) for x in edge+edge])});")
        for (l2,n2) in cast_targets(l,n):
            if l2[0]=="f" or (l2,n2)==("u8",16) or (l,n)==("u8",16): continue
            chk(f"as{tname(l2+'x'+str(n2))}",f"va.as{tname(l2+'x'+str(n2))}().as{tname(T)}()",A)
            o.append(f"  n += lanes{tname(l2+'x'+str(n2))}(\"{T}.as{tname(l2+'x'+str(n2))}\", va.as{tname(l2+'x'+str(n2))}(), {arr(l2,[wrap(l2,x) for x in A])});")
        o.append("  return n;\n}\n")
    o.append("""function bitmask(what: string, got: u64, want: u64): i64 {
  if (got != want) {
    console.log("FAIL", what, got, want);
  }
  return 1;
}

function count(what: string, got: usize, want: usize): i64 {
  if (got != want) {
    console.log("FAIL", what, got, want);
  }
  return 1;
}

function flag(what: string, got: bool, want: bool): i64 {
  if (got != want) {
    console.log("FAIL", what, got, want);
  }
  return 1;
}

function main() {""")
    for l,n in INTS:
        T=f"{l}x{n}"
        o.append(f"  console.log(\"{T}\", test{tname(T)}(), \"checks\");")
    o.append("}")
    text="\n".join(o)+"\n"
    names=[t for t in SIMD_NAMES if re.search(rf"\b{t}\b", text)]
    o.insert(4, f"import {{ {', '.join(names)} }} from \"velt:simd\";\n")
    open(f"{OUT}/simd_ints.vlt","w",encoding="utf-8",newline="\n").write("\n".join(o)+"\n")
ints()
