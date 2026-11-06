import sys,os
M=os.path.expanduser('~/velt-build/debug-inline/mp/o/')
labels=sys.argv[1:]
def load(l):
    d={}
    for x in open(M+l+'.tsv'):
        p=x.rstrip('\n').split('\t')
        if len(p)>1 and p[1]: d[p[0]]=int(p[1])
    return d
base=load('base'); T={l:load(l) for l in labels}
print('program'.ljust(16),'base'.rjust(12),*[l.rjust(8) for l in labels])
for k in base:
    print(k.ljust(16),str(base[k]).rjust(12),*[(f'{(T[l][k]/base[k]-1)*100:+.1f}%' if k in T[l] else '-').rjust(8) for l in labels])
