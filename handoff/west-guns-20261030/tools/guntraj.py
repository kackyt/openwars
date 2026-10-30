# 砲台ごとの軌跡（位置が変わった手番だけ）。使い方: guntraj.py ディレクトリ seed...
import sys,json,collections
sys.path.insert(0,__import__('os').path.dirname(__file__))
from common import *
d=sys.argv[1]
for s in sys.argv[2:]:
    r=json.load(open(f'{d}/seed{s}.json'))['results'][0]
    traj=collections.defaultdict(list)
    for h in r['strategic_history']:
        en=[u for u in h['units'] if u['player_id']==1 and u['x']<9000]
        for g in h['units']:
            if g['player_id']==2 and g['unit_type']=='砲台':
                if g['x']>9000: traj[g['unit_id']].append((h['turn'],'C',None)); continue
                dd=min([hexd((g['x'],g['y']),(e['x'],e['y'])) for e in en] or [99])
                traj[g['unit_id']].append((h['turn'],(g['x'],g['y']),dd))
    print('seed',s)
    for k,v in traj.items():
        out=[];last=None
        for t,p,dd in v:
            if p!=last: out.append(f"T{t}:{p}"+(f"[{dd}]" if dd is not None else ''))
            last=p
        print('   ',' '.join(out))
