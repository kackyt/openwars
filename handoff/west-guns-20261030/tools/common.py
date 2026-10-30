# 見積もり検証用の共通処理: ログの読み込み、ヘックス距離、地形の移動コスト
import json,glob,heapq,os
ROOT=os.path.abspath(os.path.join(os.path.dirname(__file__),"..","..",".."))
MAP=os.path.join(ROOT,"engine/src/resources/master_data/map/map_20.csv")
ROWS=[[int(c) for c in l.strip().split(',')] for l in open(MAP) if l.strip()]
H=len(ROWS); W=len(ROWS[0])
WEST=[(10,18),(6,17),(2,18),(2,19)]
EAST=[(24,13)]
def cube(x,y):
    q=x-(y-(y&1))//2; r=y; return q,r,-q-r
def hexd(a,b):
    (q1,r1,s1),(q2,r2,s2)=cube(*a),cube(*b)
    return max(abs(q1-q2),abs(r1-r2),abs(s1-s2))
def neighbors(x,y):
    for nx in range(x-1,x+2):
        for ny in range(y-1,y+2):
            if (nx,ny)!=(x,y) and 0<=nx<W and 0<=ny<H and hexd((x,y),(nx,ny))==1: yield nx,ny
# マスターデータ（movement.csv / unit.csv）から移動コストと移動力を読む
MD=os.path.join(ROOT,"engine/src/resources/master_data")+"/"
_mv=[l.strip().split(',') for l in open(MD+'movement.csv') if l.strip()]
_hdr=_mv[0][1:]
TERRAIN_NAME={1:'首都',2:'都市',3:'工場',4:'空港',5:'港',6:'道路',7:'橋',8:'平地',9:'川',10:'森',11:'山',12:'海',13:'浅瀬'}
MOVE_COST={row[0]:{_hdr[i]:int(v) for i,v in enumerate(row[1:])} for row in _mv[1:] if not row[0].startswith('移動')}
UNIT={}
for l in open(MD+'unit.csv'):
    p=l.strip().split(',')
    if len(p)>3 and p[0]!='名前': UNIT[p[0]]=(int(p[2]),p[3])
def cost(tid,kind):
    c=MOVE_COST[kind][TERRAIN_NAME[tid%100]]  # 百の位は初期所有者
    return None if c>=99 else c
def kind_of(u):
    return UNIT[u['unit_type']][1]
MOVE={name:m for name,(m,_) in UNIT.items()}
_cache={}
def dist_map(targets,kind):
    key=(tuple(targets),kind)
    if key in _cache: return _cache[key]
    d={t:0 for t in targets}; pq=[(0,t) for t in targets]
    while pq:
        c,(x,y)=heapq.heappop(pq)
        if c>d[(x,y)]: continue
        for nx,ny in neighbors(x,y):
            k=cost(ROWS[y][x],kind)  # 逆向き探索: 出るマスのコストで近似
            if k is None: continue
            nc=c+k
            if nc<d.get((nx,ny),1e9): d[(nx,ny)]=nc; heapq.heappush(pq,(nc,(nx,ny)))
    _cache[key]=d; return d
def turns_to(u,targets):
    d=dist_map(targets,kind_of(u)).get((u['x'],u['y']))
    if d is None: return 99
    m=MOVE.get(u['unit_type'],3)
    return -(-d//m)
def value(u): return u['cost']*u['hp']//100
def games(pattern):
    for f in sorted(glob.glob(pattern),key=lambda p:int(p.split('seed')[1].split('.')[0])):
        r=json.load(open(f))['results'][0]
        yield f.split('seed')[1].split('.')[0], r
def actual_arrival(hist,t,targets,player=1,radius=4,horizon=4):
    # ラウンドt〜t+horizon-1の終了時に、targetsからradius以内にいた敵の戦力の最大値
    best=0
    for k in range(t,t+horizon):
        h=hist.get(k)
        if not h: continue
        v=sum(value(u) for u in h['units'] if u['player_id']==player and u['transporting_by'] is None and min(hexd((u['x'],u['y']),c) for c in targets)<=radius)
        best=max(best,v)
    return best

def turns_blocked(u,targets,blocked):
    # blocked のマス（敵対する部隊のいるマス）を通らずに targets へ着く手番
    kind=kind_of(u); m=MOVE.get(u['unit_type'],3)
    start=(u['x'],u['y']); goal=set(targets)
    d={start:0}; pq=[(0,start)]
    while pq:
        c,(x,y)=heapq.heappop(pq)
        if (x,y) in goal: return -(-c//m)
        if c>d[(x,y)] or c>m*3: continue
        for nx,ny in neighbors(x,y):
            if (nx,ny) in blocked and (nx,ny) not in goal: continue
            k=cost(ROWS[ny][nx],kind)
            if k is None: continue
            nc=c+k
            if nc<d.get((nx,ny),1e9): d[(nx,ny)]=nc; heapq.heappush(pq,(nc,(nx,ny)))
    return 99
