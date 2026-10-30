# 変種ごと・seedごとの指標（運搬中を含む砲台・手番あたり）と、前線別の砲台の位置
import json,glob,statistics as st,sys,collections
sys.path.insert(0,__import__('os').path.dirname(__file__))
from common import *
W_=[l.strip().split(',') for l in open(MD+'weapon.csv')]; hdr=W_[0]
C={hdr[i]:(0 if v=='-' else int(v)) for i,v in enumerate(W_[[r[0] for r in W_].index('カノン砲S')]) if i>=5}
shot=lambda e: C.get(e['unit_type'],0)*e['cost']*e['hp']//10000
def area(x,y):
    if y<=9 and x<=13: return 'E_base'
    if x<=5 and 10<=y<=16: return 'corridor'
    if x<=7 and y>=16: return 'W_beyond'
    if x<=13 and y>=15: return 'W_near'
    if 14<=x<=24 and y>=14: return 'home'
    return 'right'
def stats(d,a,b):
    fire=tot=0; val=[]; out=collections.defaultdict(list); carried=0; ar=collections.Counter()
    for f in glob.glob(d+'/seed*.json'):
        for h in json.load(open(f))['results'][0]['strategic_history']:
            t=h['turn']
            if not a<=t<=b: continue
            en=[u for u in h['units'] if u['player_id']==1 and u['x']<9000]
            for g in h['units']:
                if g['player_id']==2 and g['unit_type']=='砲台':
                    tot+=1
                    if g['x']>9000: val.append(0); carried+=1; continue
                    ar[area(g['x'],g['y'])]+=1
                    v=max([shot(e) for e in en if 3<=hexd((g['x'],g['y']),(e['x'],e['y']))<=6] or [0]); val.append(v); fire+=v>0
            if t==b:
                out['p1_total'].append(sum(value(u) for u in h['units'] if u['player_id']==1))
                out['p2_total'].append(sum(value(u) for u in h['units'] if u['player_id']==2))
                out['p2_props'].append(sum(1 for p in h['properties'] if p['owner']==2))
                out['p2_right_props'].append(sum(1 for p in h['properties'] if p['owner']==2 and area(p['x'],p['y'])=='right'))
                out['p1_E_base'].append(sum(value(u) for u in h['units'] if u['player_id']==1 and u['x']<9000 and area(u['x'],u['y'])=='E_base'))
    g=len(glob.glob(d+'/seed*.json'))*(b-a+1)
    r=dict(fire=fire/tot, val=st.mean(val), carried=carried/tot, **{k:st.mean(v) for k,v in out.items()})
    for k in ('W_near','W_beyond','corridor','right'): r['gun_'+k]=ar[k]/g
    return r
variants=sys.argv[1].split(',')
for w,(a,b) in [('10',(10,19)),('22',(22,31))]:
    print(f'=== T{a}-{b}（3 seed × 12局。列は seed1 seed2 seed3 → 平均）')
    res={v:[stats(f'.tmp/gun/{v}_{w}'+('' if sd==1 else f'_s{sd}'),a,b) for sd in (1,2,3)] for v in variants}
    for k in res[variants[0]][0]:
        fmt=(lambda x:f'{x:.0%}') if k in('fire','carried') else (lambda x:f'{x/1000:.0f}k') if ('total' in k or 'E_base' in k) else (lambda x:f'{x:.2f}') if k.startswith(('p2_','gun_')) else (lambda x:f'{x:.0f}')
        print(f'  {k:15s}', ' | '.join(f"{v}: "+' '.join(fmt(r[k]) for r in res[v])+f" → {fmt(st.mean(r[k] for r in res[v]))}" for v in variants))
