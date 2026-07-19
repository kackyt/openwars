#!/usr/bin/env python3
"""
analyze_battle_log.py - battle.jsonl パース・勝敗/敗因分析サマリー出力スクリプト
"""

import sys
import json
from collections import defaultdict

def analyze_log(file_path):
    snapshots = []
    events = []
    
    with open(file_path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                data = json.loads(line)
                if data.get("event") == "TurnSnapshot":
                    snapshots.append(data)
                else:
                    events.append(data)
            except json.JSONDecodeError:
                continue

    print("# 対戦ログ分析サマリーレポート\n")
    
    # ターン数の把握
    turns = set()
    for e in events + snapshots:
        turns.add(e.get("turn", 0))
    max_turn = max(turns) if turns else 0
    print(f"- **総ターン数**: {max_turn}")
    
    # 勝敗確認
    game_over = [e for e in events if e.get("event") == "GameOver"]
    if game_over:
        print(f"- **勝敗結果**: {game_over[0].get('condition')}")
    else:
        print("- **勝敗結果**: 途中終了または未確定")
    
    print("\n## ターン別戦力・資金推移 (Snapshot)\n")
    print("| Turn | Player | 資金 | 生存ユニット数 | 占領拠点数 |")
    print("|---|---|---|---|---|")
    
    for snap in snapshots:
        turn = snap.get("turn")
        active_player = snap.get("player")
        funds = snap.get("players_funds", [])
        units = snap.get("units", [])
        props = snap.get("properties", [])
        
        # プレイヤーごとのユニット数・拠点数
        unit_counts = defaultdict(int)
        for u in units:
            unit_counts[u.get("player")] += 1
            
        prop_counts = defaultdict(int)
        for p in props:
            if p.get("owner") is not None:
                prop_counts[p.get("owner")] += 1
                
        fund_map = {p[0]: p[1] for p in funds}
        
        # 主要プレイヤー (1, 2)
        for p_id in sorted(set(list(fund_map.keys()) + list(unit_counts.keys()))):
            f_val = fund_map.get(p_id, 0)
            u_val = unit_counts.get(p_id, 0)
            pr_val = prop_counts.get(p_id, 0)
            print(f"| {turn} | Player {p_id} | {f_val} | {u_val} | {pr_val} |")

    print("\n## 生産履歴 Summary\n")
    produced = [e for e in events if e.get("event") == "UnitProduced"]
    prod_by_player = defaultdict(list)
    for p in produced:
        prod_by_player[p.get("player")].append(p.get("unit_type"))
        
    for p_id, u_list in prod_by_player.items():
        print(f"- **Player {p_id} 生産数 ({len(u_list)})**: {', '.join(u_list)}")

    print("\n## 主要撃破・損失イベント\n")
    destroyed = [e for e in events if e.get("event") == "UnitDestroyed"]
    print(f"- 総被撃破ユニット数: {len(destroyed)}")

    print("\n## AI(V3) 思考評価・ミッション推移 サマリー\n")
    ai_evals = [e for e in events if e.get("event") == "AiActionEvaluated"]
    if not ai_evals:
        print("※ AIの思考評価ログは記録されていません。")
    else:
        print(f"- 総AI評価ステップ数: {len(ai_evals)}")
        print("\n| Turn | Player | Entity | Mission | Action | Score |")
        print("|---|---|---|---|---|---|")
        for ev in ai_evals[:30]:  # 上位30件
            print(f"| {ev.get('turn')} | Player {ev.get('player')} | {ev.get('entity')} | {ev.get('mission_type')} | {ev.get('action_type')} | {ev.get('score')} |")
        if len(ai_evals) > 30:
            print(f"... 他 {len(ai_evals) - 30} 件")

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python analyze_battle_log.py <path_to_battle.jsonl>")
        sys.exit(1)
    analyze_log(sys.argv[1])
