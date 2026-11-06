import json,sys
src, name, var = sys.argv[1], sys.argv[2], sys.argv[3]
d=json.load(open(src))
out=[]
for g in d["testGroups"]:
    for t in g["tests"]:
        k=int(t["private"],16)
        priv="%064x"%k if k < 2**256 else "-"
        pub=t["public"] or "-"
        out.append(f'  "{t["tcId"]} {t["result"][0]} {priv} {pub} {t["shared"] or "-"}",')
print(f"""// Generated from Project Wycheproof's {name} (C2SP/wycheproof, testvectors_v1), one case per
// string: the case id, its result (v valid, a acceptable, i invalid), the 32-byte private key,
// the peer's public key and the shared secret; "-" is empty.

export const {var}: string[] = [""")
print("\n".join(out))
print("];")
