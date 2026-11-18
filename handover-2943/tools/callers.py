# callers.py <profile.json> <target-regex> <caller-regex> [depth]: for samples whose stack has a frame matching target,
# count the chain of the first `depth` frames above it that match caller-regex.
import json,sys,re,bisect,collections
p=json.load(open(sys.argv[1])); syms=json.load(open(sys.argv[1].replace('.json','.syms.json')))
T=re.compile(sys.argv[2]); C=re.compile(sys.argv[3]); D=int(sys.argv[4]) if len(sys.argv)>4 else 2
st=syms['string_table']; bylib={}
for d in syms['data']:
    tab=sorted(d['symbol_table'],key=lambda e:e['rva']); bylib[d['code_id'].upper()]=(([e['rva'] for e in tab]),tab)
libs=p['libs']
def name(lib,addr):
    k=bylib.get(lib['codeId'].upper())
    if not k: return lib['name']+'?'
    rvas,tab=k; i=bisect.bisect_right(rvas,addr)-1
    return re.sub(r'::h[0-9a-f]{16}$','',st[tab[i]['symbol']]) if i>=0 else '?'
cnt=collections.Counter(); total=0
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
        total+=1; chain=[]; hit=False
        while s is not None:
            n=fname(stk['frame'][s])
            if not hit and T.search(n): hit=True
            elif hit and C.search(n):
                short=re.sub(r'<|>','',n)[:90]
                if not chain or chain[-1]!=short: chain.append(short)
                if len(chain)>=D: break
            s=stk['prefix'][s]
        if hit: cnt[' <- '.join(chain)]+=1
print('total',total)
for k,v in cnt.most_common(40): print(f'{100*v/total:5.1f}% {k}')
