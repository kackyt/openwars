import json
import os
import sys

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
SKILL_DIR = os.path.dirname(SCRIPT_DIR)
SCRATCH_DIR = os.path.join(SKILL_DIR, "scratch")
BOARD_STATE_PATH = os.path.join(SCRATCH_DIR, "board_state.json")
RECOMMENDATIONS_PATH = os.path.join(SCRATCH_DIR, "recommendations.json")

def get_manhattan_distance(x1, y1, x2, y2):
    return abs(x1 - x2) + abs(y1 - y2)

def analyze_board():
    try:
        with open(BOARD_STATE_PATH, 'r', encoding='utf-8') as f:
            board_state = json.load(f)
    except FileNotFoundError:
        print(f"Error: {BOARD_STATE_PATH} not found.", file=sys.stderr)
        sys.exit(1)

    turn = board_state.get('turn', 1)
    active_player_index = board_state.get('active_player_index', 0)
    players = board_state.get('players', [])
    units = board_state.get('units', [])
    properties = board_state.get('properties', [])

    if not players or active_player_index >= len(players):
        active_player_id = 0
    else:
        active_player_id = players[active_player_index].get('player_id', 0)

    my_units = [u for u in units if u.get('player_id') == active_player_id]
    enemy_units = [u for u in units if u.get('player_id') != active_player_id]

    recommendations = []

    # 簡易的なサマリー生成
    summary = f"ターン{turn}です。自軍のユニットは{len(my_units)}体、敵軍のユニットは{len(enemy_units)}体です。"
    if len(my_units) > len(enemy_units):
         summary += " 自軍が優勢な状況です。"
    elif len(my_units) < len(enemy_units):
         summary += " 敵軍が優勢な状況です。注意して進軍してください。"
    else:
         summary += " 互角の戦況です。"

    for unit in my_units:
        unit_id = unit.get('unit_id')
        unit_name = unit.get('unit_type', 'Unknown')
        ux = unit.get('x')
        uy = unit.get('y')

        # 敵ユニットとの距離を計算
        closest_enemy = None
        min_enemy_dist = float('inf')
        for enemy in enemy_units:
            ex = enemy.get('x')
            ey = enemy.get('y')
            dist = get_manhattan_distance(ux, uy, ex, ey)
            if dist < min_enemy_dist:
                min_enemy_dist = dist
                closest_enemy = enemy

        # 拠点との距離を計算 (自軍以外の拠点)
        closest_prop = None
        min_prop_dist = float('inf')
        for prop in properties:
            owner = prop.get('owner')
            if owner != active_player_id: # 中立(None)または敵軍
                px = prop.get('x')
                py = prop.get('y')
                dist = get_manhattan_distance(ux, uy, px, py)
                if dist < min_prop_dist:
                    min_prop_dist = dist
                    closest_prop = prop

        action_rec = {
            "unit_id": unit_id,
            "unit_name": unit_name,
        }

        # ヒューリスティクス評価
        # 1. 敵が隣接しているか、移動＋攻撃可能な距離にある場合 (単純化して距離3以内を攻撃可能と仮定せず、距離に基づいて判断)
        # 実際にはユニットの移動力＋射程に依存するが、ここでは簡易的な距離ベースとする。
        # issueの要件「敵ユニットを攻撃可能な位置にいる、または移動して攻撃できる場合は攻撃アクションを推奨する。」
        # 距離が近い(例えば1〜4程度)なら攻撃可能とみなして推奨する。
        if closest_enemy and min_enemy_dist <= 3:
            ex = closest_enemy.get('x')
            ey = closest_enemy.get('y')

            # 既に隣接している場合は、移動せずにその場から攻撃する
            if min_enemy_dist == 1:
                target_x = ux
                target_y = uy
            else:
                # 敵の隣接4方向の座標候補
                candidates = [
                    (ex - 1, ey),
                    (ex + 1, ey),
                    (ex, ey - 1),
                    (ex, ey + 1)
                ]

                # 自分以外の他のユニットがいる座標を特定
                occupied = set((u.get('x'), u.get('y')) for u in units if u.get('unit_id') != unit_id)

                # properties と units の最大値からマップサイズ（概算）を推測
                max_x = max([p.get('x', 0) for p in properties] + [u.get('x', 0) for u in units] + [10])
                max_y = max([p.get('y', 0) for p in properties] + [u.get('y', 0) for u in units] + [10])

                # 範囲内で、かつ他のユニットに占有されていない隣接セルをフィルタ
                valid_candidates = []
                for cx, cy in candidates:
                    if 0 <= cx <= max_x and 0 <= cy <= max_y:
                        if (cx, cy) not in occupied:
                            valid_candidates.append((cx, cy))

                if valid_candidates:
                    # 自軍ユニット(ux, uy)から最も近い隣接セルを選択
                    valid_candidates.sort(key=lambda coord: get_manhattan_distance(ux, uy, coord[0], coord[1]))
                    target_x, target_y = valid_candidates[0]
                else:
                    # 空きマスがない場合はフォールバックとして最も近い隣接セルを選択
                    candidates.sort(key=lambda coord: get_manhattan_distance(ux, uy, coord[0], coord[1]))
                    bounded = [(cx, cy) for cx, cy in candidates if 0 <= cx <= max_x and 0 <= cy <= max_y]
                    if bounded:
                        target_x, target_y = bounded[0]
                    else:
                        target_x, target_y = candidates[0]

            action_rec["action_type"] = "MoveAndAttack"
            action_rec["target_x"] = target_x
            action_rec["target_y"] = target_y
            action_rec["target_id"] = closest_enemy.get('unit_id')
            action_rec["explanation"] = f"({ex},{ey})の敵ユニットを攻撃するため、({target_x},{target_y})へ移動して攻撃することを推奨します。"

        # 2. 中立または敵の拠点を占領可能な位置にいる場合
        elif closest_prop and min_prop_dist <= 1:
            action_rec["action_type"] = "Capture"
            action_rec["target_x"] = closest_prop.get('x')
            action_rec["target_y"] = closest_prop.get('y')
            action_rec["explanation"] = f"({closest_prop.get('x')},{closest_prop.get('y')})の拠点が近くにあるため、移動して占領（または占領開始）を推奨します。"

        # 3. それ以外の場合、最も近い敵ユニットや敵の拠点に向かって進軍する移動アクションを推奨する。
        else:
            action_rec["action_type"] = "Move"

            # 敵と拠点のどちらに近いかで目標を決める
            if closest_enemy and (not closest_prop or min_enemy_dist < min_prop_dist):
                target = closest_enemy
                action_rec["explanation"] = f"最も近い敵ユニットがいる({target.get('x')},{target.get('y')})の方向へ進軍することを推奨します。"
            elif closest_prop:
                target = closest_prop
                action_rec["explanation"] = f"最も近い未占領・敵拠点の({target.get('x')},{target.get('y')})の方向へ進軍することを推奨します。"
            else:
                target = {"x": ux, "y": uy}
                action_rec["explanation"] = "目標が見つからないため、待機を推奨します。"
                action_rec["action_type"] = "Wait"

            action_rec["target_x"] = target.get('x')
            action_rec["target_y"] = target.get('y')

        recommendations.append(action_rec)

    output = {
        "summary": summary,
        "recommendations": recommendations
    }

    os.makedirs(SCRATCH_DIR, exist_ok=True)
    with open(RECOMMENDATIONS_PATH, 'w', encoding='utf-8') as f:
        json.dump(output, f, ensure_ascii=False, indent=2)

if __name__ == "__main__":
    analyze_board()
