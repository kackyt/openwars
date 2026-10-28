import json,sys,collections
# 作戦ごとの計画（実行可能か・敵数・今買うもの）と左下拠点の推移を出す
path=sys.argv[1]; me=int(sys.argv[2]) if len(sys.argv)>2 else 2
d=json.load(open(path)); r=d['results'][0]
print(r['result'], r['metrics'][-1]['p1_props'], r['metrics'][-1]['p2_props'])
for h in r['production_plan_history']:
    if h['player_id']!=me: continue
    rows=[]
    for p in h['plan']['rolling_combat_plans']:
        now=collections.Counter(x['unit_type'] for x in p['purchases'] if x['build_turn']==0)
        rows.append(f"({p['anchor_x']},{p['anchor_y']}) n={len(p['targets'])} feas={'Y' if p['feasible'] else 'n'} occ={p['occupation_turn']} now={dict(now)}")
    print(f"T{h['turn']}", ' | '.join(rows))
cells=[(6,17),(2,18),(2,19),(10,18),(20,3),(21,5),(26,4)]
hist={}
for h in r['strategic_history']:
    for p in h['properties']:
        k=(p['x'],p['y'])
        if k in cells: hist.setdefault(k,[]).append((h['turn'],p['owner']))
for k in cells:
    ch=[];prev=None
    for t,o in hist.get(k,[]):
        if o!=prev: ch.append(f'T{t}:P{o}'); prev=o
    print(k,' '.join(ch))
