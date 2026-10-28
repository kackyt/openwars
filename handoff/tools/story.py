import json,sys,collections,re
f=sys.argv[1]; me=2
r=json.load(open(f))['results'][0]
print(f.split('/')[-3], f.split('/')[-1], r['result'])
# 拠点・戦力・資金の推移
for m in r['metrics']:
    if m['turn'] in (1,3,5,8,10,12,15,18,20,25,30):
        print(f" T{m['turn']:2} props P1={m['p1_props']} P2={m['p2_props']}  units$ P1={m['p1_units']} P2={m['p2_units']}  funds P1={m['p1_funds']} P2={m['p2_funds']}")
# 生産と輸送の要点
prod=collections.defaultdict(collections.Counter); ferry=[]
for h in r['action_history']:
    if h['player_id']!=me: continue
    for a in h['actions']:
        m=re.search(r'unit_type: (\w+)',a)
        if 'Produce' in a and m: prod[h['turn']][m.group(1)]+=1
        elif a.startswith('Load') or a.startswith('Drop'):
            ferry.append(f"T{h['turn']}:{re.sub(r'GridPosition \{ x: (\d+), y: (\d+) \}',r'(\1,\2)',a)[:70]}")
tot=collections.Counter()
for t in sorted(prod): tot+=prod[t]
print(' early prod:', {t:dict(prod[t]) for t in sorted(prod) if t<=4})
print(' total prod:', dict(tot.most_common()))
print(' ferry:', ferry[:12])
# 作戦の推移
ops=[]
for h in r['production_plan_history']:
    if h['player_id']!=me: continue
    ops.append(f"T{h['turn']}:"+'/'.join(f"{p['operation_kind'][0]}({p['anchor_x']},{p['anchor_y']})" for p in h['plan']['rolling_combat_plans']))
print(' ops:', ' '.join(ops[:30]))
# 重要拠点の所有推移
cells={(10,18):'橋前都市',(6,17):'橋向こう都市',(2,18):'西端',(2,19):'西端2',(26,4):'右上',(21,5):'上中',(20,3):'上中2',(7,10):'左中P1都市'}
hist={}
for h in r['strategic_history']:
    for p in h['properties']:
        k=(p['x'],p['y'])
        if k in cells: hist.setdefault(k,[]).append((h['turn'],p['owner']))
for k,n in cells.items():
    ch=[];prev=None
    for t,o in hist.get(k,[]):
        if o!=prev: ch.append(f'T{t}:P{o}'); prev=o
    print(f'  {n}{k}',' '.join(ch))
