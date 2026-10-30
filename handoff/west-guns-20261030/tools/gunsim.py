# 分岐シミュレーション（または対局ログ）の砲台の指標をラベルごとに並べる。
# 使い方: gunsim.py <ラウンドa-b> ラベル=ディレクトリ ...
import sys,collections,statistics as st,glob,json
sys.path.insert(0,__import__('os').path.dirname(__file__))
from common import *
GUNOK=lambda c: TERRAIN_NAME[ROWS[c[1]][c[0]]%100] not in ('川','森','山','海','浅瀬')
W_=[l.strip().split(',') for l in open(MD+'weapon.csv')]; hdr=W_[0]
C={hdr[i]:(0 if v=='-' else int(v)) for i,v in enumerate(W_[[r[0] for r in W_].index('カノン砲S')]) if i>=5}
def shot(e): return C.get(e['unit_type'],0)*e['cost']*e['hp']//10000
def area(c):
    x,y=c
    if y<=9 and x<=13: return 'E_base'
    if x<=5 and 10<=y<=16: return 'corridor'
    if x<=7 and y>=16: return 'W_beyond'
    if x<=13 and y>=15: return 'W_near'
    if 14<=x<=24 and y>=14: return 'home'
    return 'right'
a,b=map(int,sys.argv[1].split('-'))
for arg in sys.argv[2:]:
    label,d=arg.split('=')
    cnt=collections.Counter(); val=[]; ar=collections.Counter(); n=0
    out=collections.defaultdict(list)
    for f in glob.glob(d+'/seed*.json'):
        r=json.load(open(f))['results'][0]
        for h in r['strategic_history']:
            t=h['turn']
            if not a<=t<=b: continue
            n+=1
            us=[u for u in h['units'] if u['x']<9000]
            en=[u for u in us if u['player_id']==1]
            car=[(u['x'],u['y']) for u in us if u['player_id']==2 and u['unit_type']=='補給輸送車']
            for g in h['units']:
                if g['player_id']!=2 or g['unit_type']!='砲台': continue
                if g['x']>9000: ar['carried']+=1; continue
                c=(g['x'],g['y']); ar[area(c)]+=1; cnt['all']+=1
                v=max([shot(e) for e in en if 3<=hexd(c,(e['x'],e['y']))<=6] or [0]); val.append(v)
                if v>0: cnt['fire']+=1; continue
                if not any(GUNOK(x) for x in neighbors(*c)): cnt['trap']+=1; continue
                adj=[q for q in car if hexd(q,c)==1]
                if adj and not any(GUNOK(q) for q in adj): cnt['forest_tender']+=1; continue
                cnt['idle_other']+=1
            if t==b:
                out['west_cities'].append(sum(1 for p in h['properties'] if (p['x'],p['y']) in WEST and p['owner']==2))
                out['p2_props'].append(sum(1 for p in h['properties'] if p['owner']==2))
                for pl in (1,2):
                    for z in ('E_base','corridor','W_beyond','W_near','right','home'):
                        out[f'p{pl}_{z}'].append(sum(value(u) for u in us if u['player_id']==pl and area((u['x'],u['y']))==z))
                    out[f'p{pl}_total'].append(sum(value(u) for u in h['units'] if u['player_id']==pl))
    tot=max(cnt['all'],1); games=len(glob.glob(d+'/seed*.json')); rounds=b-a+1
    print(f"== {label}: T{a}-{b}  {games}局")
    print("  砲台の位置(1局1手番あたり):",{k:round(v/(games*rounds),2) for k,v in sorted(ar.items())})
    print("  撃てる:",f"{cnt['fire']/tot:.0%}","乗り直し不能:",f"{cnt['trap']/tot:.0%}","森の付き添い:",f"{cnt['forest_tender']/tot:.0%}","その他待機:",f"{cnt['idle_other']/tot:.0%}","  最大損害価値の平均:",round(st.mean(val)) if val else 0)
    print(f"  T{b}終了時:",' '.join(f"{k}={st.mean(v)/1000:.0f}k" if k.startswith('p') and 'props' not in k else f"{k}={st.mean(v):.1f}" for k,v in out.items()))
