import json,glob,sys
from collections import defaultdict
root=sys.argv[1]
res=defaultdict(dict); variants=set()
for f in glob.glob(f'{root}/games/*/*/seed*.json'):
    parts=f.split('/'); var=parts[-3]; cond=parts[-2]; seed=parts[-1]
    variants.add(var)
    g=json.load(open(f))
    if 'results' in g: g=g['results'][0]
    v4=1 if g['p1']=='V4' else 2
    side='p1' if v4==1 else 'p2'; other='p2' if v4==1 else 'p1'
    m=g['metrics'][-1]
    res[(cond,seed)][var]=(g['result'].startswith(f'P{v4}_Win'), m[side+'_props']-m[other+'_props'])
variants=sorted(variants)
tot=defaultdict(lambda:[0,0,0])
for k in sorted(res):
    row=res[k]
    print(k, '  '.join(f"{v}: {'W' if row[v][0] else 'L'} {row[v][1]:+d}" for v in variants if v in row))
    for v in row:
        t=tot[(k[0].split('_V')[0],v)]; t[0]+=row[v][0]; t[1]+=row[v][1]; t[2]+=1
print()
for k in sorted(tot): print(k, 'wins',tot[k][0],'/',tot[k][2],'props_diff_sum',tot[k][1])
