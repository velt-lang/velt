import json,sys
src, var = sys.argv[1], sys.argv[2]
d=json.load(open(src))
out=[]
for g in d["testGroups"]:
    out.append(f'  "K {g["publicKey"]["uncompressed"]}",')
    for t in g["tests"]:
        out.append(f'  "{t["tcId"]} {t["result"][0]} {t["msg"] or "-"} {t["sig"] or "-"}",')
name=src.split("/")[-1]
print(f"""// Generated from Project Wycheproof's {name} (C2SP/wycheproof,
// testvectors_v1): "K <key>" starts a group with its uncompressed public key, then one case per
// string: the case id, its result (v valid, a acceptable, i invalid), the message and the DER
// signature; "-" is empty.

export const {var}: string[] = [""")
print("\n".join(out))
print("];")
