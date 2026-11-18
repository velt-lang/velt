# selfunder.py <profile.json> <ancestor-regex> [N]: self time of leaf functions in samples under a frame matching ancestor.
import json,sys,re,bisect,collections
p=json.load(open(sys.argv[1])); syms=json.load(open(sys.argv[1].replace('.json','.syms.json')))
A=re.compile(sys.argv[2]); N=int(sys.argv[3]) if len(sys.argv)>3 else 30
st=syms['string_table']; bylib={}
for d in syms['data']:
    tab=sorted(d['symbol_table'],key=lambda e:e['rva']); bylib[d['code_id'].upper()]=(([e['rva'] for e in tab]),tab)
libs=p['libs']
def name(lib,addr):
    k=bylib.get(lib['codeId'].upper())
    if not k: return lib['name']+'?'
    rvas,tab=k; i=bisect.bisect_right(rvas,addr)-1
    return re.sub(r'::h[0-9a-f]{16}$','',st[tab[i]['symbol']]) if i>=0 else '?'
cnt=collections.Counter(); under=0
for th in p['threads']:
    ft,stk,sm,rt,fn=th['frameTable'],th['stackTable'],th['samples'],th['resourceTable'],th['funcTable']
    cache={}
    def fname(f):
        if f not in cache:
            res=fn['resource'][ft['func'][f]]
            cache[f]=name(libs[rt['lib'][res]],ft['address'][f]) if res is not None and res>=0 and rt['lib'][res] is not None else th['stringArray'][fn['name'][ft['func'][f]]]
        return cache[f]
    for s in sm['stack']:
        if s is None: continue
        frames=[]; x=s
        while x is not None: frames.append(fname(stk['frame'][x])); x=stk['prefix'][x]
        if any(A.search(f) for f in frames):
            under+=1
            # leaf, plus first velt frame above it
            leaf=frames[0]; owner=next((f for f in frames if re.search(r'velt|vpm',f)),'?')
            cnt[(leaf[:60],owner[:80])]+=1
print('samples under',under)
for (l,o),v in cnt.most_common(N): print(f'{v:5d} {l:60} <- {o}')
