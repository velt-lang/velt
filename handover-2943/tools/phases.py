# phases.py <profile.json> name=regex ...: share of non-idle samples under each (first match from the leaf up wins? no: outermost listed phase order = priority)
import json,sys,re,bisect,collections
p=json.load(open(sys.argv[1])); syms=json.load(open(sys.argv[1].replace('.json','.syms.json')))
phases=[(a.split('=',1)[0],re.compile(a.split('=',1)[1])) for a in sys.argv[2:]]
st=syms['string_table']; bylib={}
for d in syms['data']:
    tab=sorted(d['symbol_table'],key=lambda e:e['rva']); bylib[d['code_id'].upper()]=(([e['rva'] for e in tab]),tab)
libs=p['libs']
def name(lib,addr):
    k=bylib.get(lib['codeId'].upper())
    if not k: return lib['name']+'?'
    rvas,tab=k; i=bisect.bisect_right(rvas,addr)-1
    return re.sub(r'::h[0-9a-f]{16}$','',st[tab[i]['symbol']]) if i>=0 else '?'
IDLE=re.compile(r'__ulock_wait|__psynch_cvwait|__semwait|kevent|__workq_kernreturn|mach_msg|__psynch_mutexwait|_pthread_join|pthread_join')
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
        frames=[]; x=s
        while x is not None: frames.append(fname(stk['frame'][x])); x=stk['prefix'][x]
        if IDLE.search(frames[0]): continue
        total+=1
        hit='other'
        for nm,rx in phases:
            if any(rx.search(f) for f in frames): hit=nm; break
        cnt[hit]+=1
print('non-idle samples',total)
for k,v in cnt.most_common(): print(f'{100*v/total:5.1f}%  {v:5d}  {k}')
