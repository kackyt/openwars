# T20の西の都市と橋の詰まりを版ごとに比べる。使い方: t20.py 版...
import json,glob,sys,collections,statistics as st
sys.path.insert(0,__import__('os').path.dirname(__file__))
from common import *
FERRY={'砲台','補給輸送車'}
for v in sys.argv[1:]:
    w20=[];all4=0;n=0;per=collections.Counter();own=[];en=[];bridge=collections.Counter();bt=0
    for X in ('','_s2','_s3'):
        for f in glob.glob(f'.tmp/gun/t20_{v}{X}/seed*.json'):
            hs={h['turn']:h for h in json.load(open(f))['results'][0]['strategic_history']}
            h=hs[20]; n+=1
            props={(p['x'],p['y']):p['owner'] for p in h['properties']}
            k=sum(1 for c in WEST if props.get(c)==2); w20.append(k); all4+=k==4
            for c in WEST: per[c]+=props.get(c)==2
            own.append(sum(value(u) for u in h['units'] if u['player_id']==2)); en.append(sum(value(u) for u in h['units'] if u['player_id']==1))
            for t in range(12,21):
                b=[u for u in hs[t]['units'] if (u['x'],u['y'])==(8,18) and u['player_id']==2]
                bt+=1; bridge['輸送']+=bool(b and b[0]['unit_type'] in FERRY); bridge['他']+=bool(b and b[0]['unit_type'] not in FERRY)
    print(f"{v:5s} T20 西の都市 {st.mean(w20):.2f}/4  4つ全部 {all4}/{n}局  (6,17){per[(6,17)]} (2,18){per[(2,18)]} (2,19){per[(2,19)]}  自軍 {st.mean(own)/1000:.0f}k 敵 {st.mean(en)/1000:.0f}k  橋の上(T12-20): 砲台/輸送車 {bridge['輸送']/bt:.0%} 他 {bridge['他']/bt:.0%}")
