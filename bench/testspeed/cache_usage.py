#!/usr/bin/env python3
"""The Actions cache by scope and kind: entries, size, and how fast it grew. Reads
`gh api repos/.../actions/caches` pages from stdin-free calls. usage: cache_usage.py"""
import json, subprocess, collections, datetime as d
rows=[]; page=1
while True:
    o=subprocess.run(["gh","api",f"repos/velt-lang/velt/actions/caches?per_page=100&page={page}&sort=created_at&direction=desc"],capture_output=True,text=True)
    j=json.loads(o.stdout); c=j.get("actions_caches",[])
    rows+=c
    if len(c)<100 or page>=60: break
    page+=1
print("listed",len(rows),"total_count",j.get("total_count"))
def kind(k):
    return "sccache" if k.startswith("sccache") else "clang" if k.startswith("velt-clang") else "rust" if "-rust-" in k else "other"
by=collections.Counter(); n=collections.Counter()
for r in rows:
    ref=r["ref"]; s="main" if ref=="refs/heads/main" else "queue" if "gh-readonly-queue" in ref else "pr" if ref.startswith("refs/pull") else "branch"
    by[(s,kind(r["key"]))]+=r["size_in_bytes"]; n[(s,kind(r["key"]))]+=1
for k,v in sorted(by.items(),key=lambda x:-x[1]): print(k,n[k],"entries",v>>20,"MiB")
sc=[r for r in rows if kind(r["key"])=="sccache"]
per=collections.Counter(); perb=collections.Counter()
for r in sc: per[r["ref"]]+=1; perb[r["ref"]]+=r["size_in_bytes"]
print("sccache by ref, top:")
for k,v in perb.most_common(12): print(" ",k,per[k],v>>20,"MiB")
bh=collections.Counter()
for r in sc:
    bh[r["created_at"][:13]]+=r["size_in_bytes"]
print("sccache per hour:",[(k[5:],v>>20) for k,v in sorted(bh.items())])
print("oldest rust last used:",sorted(r["last_accessed_at"] for r in rows if kind(r["key"])=="rust")[:2])
print("biggest sccache:",sorted((r["size_in_bytes"]>>20 for r in sc),reverse=True)[:8])
