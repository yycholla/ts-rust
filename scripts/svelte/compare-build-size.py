#!/usr/bin/env python3
"""Usage: compare-build-size.py OFFICIAL_BUILD NATIVE_BUILD [--json OUT]
Compare two adapter-node build/ directories by area (client JS/CSS/maps,
precompressed files, server JS/maps) and print a table of byte counts.
"""
import json,os,sys
from pathlib import Path
if len(sys.argv) not in (3,5) or sys.argv[1] in ('help','--help'):
 print(__doc__);sys.exit(0 if len(sys.argv)>1 and sys.argv[1] in ('help','--help') else 2)
def area(rel):
 top='client' if rel.startswith('client/') else 'server' if rel.startswith('server/') else 'adapter'
 if rel.endswith(('.gz','.br')):return top+' precompressed (.gz/.br)'
 if rel.endswith('.map'):return top+' source maps'
 if rel.endswith(('.js','.mjs')):return top+' JS'
 if rel.endswith('.css'):return top+' CSS'
 return top+' other assets'
def scan(root):
 sizes={};counts={};gz=0;total=0
 for dp,_,fs in os.walk(root):
  for f in fs:
   p=Path(dp)/f;rel=str(p.relative_to(root));n=p.stat().st_size;a=area(rel)
   sizes[a]=sizes.get(a,0)+n;counts[a]=counts.get(a,0)+1;total+=n
   if rel.startswith('client/') and rel.endswith('.gz') and rel.endswith(('.js.gz','.mjs.gz','.css.gz')):gz+=n
 return sizes,counts,total,gz
o=scan(Path(sys.argv[1]));n=scan(Path(sys.argv[2]))
rows=sorted(set(o[0])|set(n[0]))
print(f"{'area':34s} {'official':>12s} {'native':>12s} {'delta':>8s}   files o/n")
for a in rows+['TOTAL','client JS+CSS gzip (shipped)']:
 if a=='TOTAL':ov,nv,oc,nc=o[2],n[2],sum(o[1].values()),sum(n[1].values())
 elif a.startswith('client JS+CSS gzip'):ov,nv,oc,nc=o[3],n[3],0,0
 else:ov,nv,oc,nc=o[0].get(a,0),n[0].get(a,0),o[1].get(a,0),n[1].get(a,0)
 d=f"{(nv-ov)/ov*100:+.1f}%" if ov else 'new'
 print(f"{a:34s} {ov:12,d} {nv:12,d} {d:>8s}   {oc}/{nc}")
if len(sys.argv)==5:
 Path(sys.argv[4]).write_text(json.dumps({'official':{'areas':o[0],'files':o[1],'total':o[2],'clientGzip':o[3]},'native':{'areas':n[0],'files':n[1],'total':n[2],'clientGzip':n[3]}},indent=1)+'\n')
