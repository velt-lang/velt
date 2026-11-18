# prof.py <profile.json> [N] [filter-regex]: samply profile -> inclusive and self time per function (all threads).
import json,sys,re,bisect,collections
p=json.load(open(sys.argv[1])); syms=json.load(open(sys.argv[1].replace('.json','.syms.json')))
N=int(sys.argv[2]) if len(sys.argv)>2 else 40; flt=re.compile(sys.argv[3]) if len(sys.argv)>3 else None
st=syms['string_table']; bylib={}
for d in syms['data']:
    tab=sorted(d['symbol_table'],key=lambda e:e['rva']); bylib[d['code_id'].upper()]=(([e['rva'] for e in tab]),tab)
libs=p['libs']
def name(lib,addr):
    k=bylib.get(lib['codeId'].upper())
    if not k: return lib['name']+'?'
    rvas,tab=k; i=bisect.bisect_right(rvas,addr)-1
    if i<0: return '?'
    s=st[tab[i]['symbol']]
    s=re.sub(r'::h[0-9a-f]{16}$','',s); return s
incl=collections.Counter(); selfc=collections.Counter(); total=0
for th in p['threads']:
    ft,stk,sm,rt=th['frameTable'],th['stackTable'],th['samples'],th['resourceTable']; fn=th['funcTable']
    cache={}
    def fname(f):
        if f in cache: return cache[f]
        res=fn['resource'][ft['func'][f]]
        n=name(libs[rt['lib'][res]],ft['address'][f]) if res is not None and res>=0 and rt['lib'][res] is not None else th['stringArray'][fn['name'][ft['func'][f]]]
        cache[f]=n; return n
    w=sm.get('weight') or [1]*sm['length']
    for s,wt in zip(sm['stack'],w):
        if s is None: continue
        wt=wt or 1; total+=wt; seen=set(); first=True
        while s is not None:
            n=fname(stk['frame'][s])
            if first: selfc[n]+=wt; first=False
            if n not in seen: incl[n]+=wt; seen.add(n)
            s=stk['prefix'][s]
print('total samples',total)
def show(c,title):
    print('==',title)
    for n,v in c.most_common(100000):
        if flt and not flt.search(n): continue
        print(f'{100*v/total:6.1f}%  {n[:150]}'); 
        show.k+=1
        if show.k>=N: break
    show.k=0
show.k=0
show(incl,'inclusive'); show(selfc,'self')
