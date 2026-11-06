import re,sys
lines=open(sys.argv[1]).read().split("\n")
start,end=int(sys.argv[2]),int(sys.argv[3])
L=[l for l in lines[start-1:end] if not l.startswith("RFC 8448") and not l.startswith("Thomson") and "\f" not in l]
blocks=[];actor="";step=""
i=0
while i<len(L):
    l=L[i]
    m=re.match(r"^   \{(\w+)\}\s+(.*)",l)
    if m: actor=m.group(1); step=m.group(2)
    m=re.match(r"^      ([^:]+?)(?: \((\d+) octets\))?:\s+((?:[0-9a-f]{2} ?)+)$",l)
    if m:
        hexs=m.group(3).replace(" ","")
        j=i+1
        while j<len(L) and re.match(r"^\s+((?:[0-9a-f]{2} ?)+)$",L[j]) and not re.match(r"^      \S",L[j]):
            hexs+=L[j].strip().replace(" ",""); j+=1
        blocks.append((actor,step,m.group(1),hexs)); i=j; continue
    i+=1
for b in blocks: print(b[0],"|",b[1],"|",b[2],"|",len(b[3])//2,"|",b[3][:40])
import json; json.dump(blocks,open(sys.argv[4],"w"))
