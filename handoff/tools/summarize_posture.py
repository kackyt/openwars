import re,sys,collections
# 配分案の再計算ターン・候補の見通し・選択と、ターンごとの生産をまとめる
path=sys.argv[1]; me=sys.argv[2]  # me: "1" or "2"
turn=None; out=[]; prod=collections.defaultdict(collections.Counter); last_key=None
pos=lambda t: re.sub(r'GridPosition \{ x: (\d+), y: (\d+) \}',r'(\1,\2)',t)
for line in open(path,errors='ignore'):
    m=re.match(r'^T(\d+) PPlayerId\((\d)\)',line)
    if m:
        turn=int(m.group(1)); who=m.group(2)
        if who==me:
            u=re.search(r'unit_type: (\w+)',line)
            if 'Produce' in line and u: prod[turn][u.group(1)]+=1
        continue
    if line.startswith('ROUTE_POSTURE_INPUT'):
        anchors=re.search(r'anchors=(\[.*?\]) enemy',pos(line)).group(1)
        cur_anchors=anchors
    if line.startswith('ROUTE_POSTURE prospects'):
        l=pos(line)
        pros=re.findall(r'own: (\d+), enemy: (\d+), gains: (\d+), losses: (\d+), capital_turns: (\w+(?:\(\d+\))?), exposed_capital_turns: (\w+(?:\(\d+\))?)',l)
        ch=re.search(r'chosen=(\S+)',l).group(1); sp=re.search(r'spread=(\S+)',l).group(1); sc=re.search(r'score=(\S+)',l).group(1)
        b=re.search(r'own: (\d+), enemy: (\d+) \}',l)
        key=(turn,ch,cur_anchors)
        if key==last_key: continue
        last_key=key
        anchors=re.findall(r'Some\(\((\d+,\d+)\)\)|None',cur_anchors)
        out.append(f"T{turn}: routes={cur_anchors} chosen={ch} score={sc} spread={sp} budget(own/enemy)={b.group(1)}/{b.group(2)}")
        for i,p in enumerate(pros):
            out.append(f"     route{i}: own={p[0]} enemy={p[1]} gains={p[2]} losses={p[3]} to_enemy_cap={p[4]} enemy_to_our_cap={p[5]}")
print('\n'.join(out))
print('production:', '; '.join(f"T{t}:"+','.join(f"{k}{v}" for k,v in prod[t].most_common()) for t in sorted(prod) if t<=12))
