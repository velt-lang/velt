import json,sys
m=json.load(open('covfull/matrix.json'))
comp={r:set(v['marks']) for r,v in m.items() if v['code']==0}
allm=set().union(*comp.values())
def fnv(s):
    h=0xcbf29ce484222325
    for b in s.encode():
        h=((h^b)*0x100000001b3)&(2**64-1)
    return h
for n in (8,16,32,64):
    s=[r for r in m if fnv(r)%n==0]
    cov=set().union(*(comp.get(r,set()) for r in s))
    print('stride',n,len(s),'programs, compiled',sum(r in comp for r in s),'missing',sorted(allm-cov))
n=16
s=[r for r in m if fnv(r)%n==0]
cov=set().union(*(comp.get(r,set()) for r in s))
extra=[]
while allm-cov:
    # the smallest-marks program covering the most remaining
    best=max(comp,key=lambda r:(len(comp[r]&(allm-cov)),-len(comp[r])))
    extra.append((best,sorted(comp[best]&(allm-cov))))
    cov|=comp[best]
print('extra for stride 16',extra)
for b in sorted(allm,key=lambda x:sum(x in c for c in comp.values()))[:8]:
    print(b,[r for r,c in comp.items() if b in c][:6])
print('closure',sorted(comp.get('lang/closure_nested_assigns_capture.vlt',[])))
print('total marks',len(allm),'compiled',len(comp),'not compiled',len(m)-len(comp))
print('avg marks',sum(len(c) for c in comp.values())/len(comp))
