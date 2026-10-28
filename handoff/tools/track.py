import re,collections,sys
S=sys.argv[1]
t=None; seen={}
for line in open(S,errors='ignore'):
    m=re.match(r'^T(\d+) PPlayerId',line)
    if m: t=int(m.group(1)); continue
    if line.startswith('TACTIC P2'):
        e=line.split()[2]; at=re.search(r'at=\((\d+),(\d+)\)',line)
        tg=re.search(r'target=Some\(GridPosition \{ x: (\d+), y: (\d+) \}\)',line)
        mi=re.search(r'mission=Some\((\w+)\)',line)
        if at: seen.setdefault(e,{})[t]=(int(at.group(1)), f"{mi.group(1)[0] if mi else '-'}({tg.group(1)},{tg.group(2)})" if tg else None)
for turn in range(2,15):
    c=collections.Counter(); xs=collections.defaultdict(list)
    for e,d in seen.items():
        if turn in d:
            x,tg=d[turn]; c[tg]+=1; xs[tg].append(x)
    if c: print(f"T{turn}", {k:(v, round(sum(xs[k])/len(xs[k]),1)) for k,v in c.most_common()})
sw=[sum(1 for a,b in zip(sorted(d),sorted(d)[1:]) if d[a][1]!=d[b][1]) for d in seen.values()]
print('units',len(seen),'target switches',sum(sw))
west=[e for e,d in seen.items() if 3 in d and d[3][1] and '6,17' in d[3][1]]
for e in west[:4]:
    d=seen[e]; print(e, ' '.join(f"T{k}:x{v[0]}{v[1]}" for k,v in sorted(d.items())))
