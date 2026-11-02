#!/usr/bin/env python3
"""自手番前後の検証記録から、指示と実進入・生産口封鎖・砲撃後の前進を分けて集計する。"""

import argparse
import csv
from collections import Counter
import json
from pathlib import Path


def hex_distance(first, second):
    """ゲームと同じ odd-r offset のHex距離。"""
    def cube(point):
        q = point["x"] - (point["y"] - (point["y"] & 1)) // 2
        return q, point["y"], -q - point["y"]

    return sum(abs(a - b) for a, b in zip(cube(first), cube(second))) // 2


def position(unit):
    return unit["x"], unit["y"]


def review(path):
    data = json.loads(path.read_text())
    snapshots = data["snapshots"]
    master = Path(__file__).resolve().parents[1] / "engine/src/resources/master_data"
    with (master / "category.csv").open() as source:
        ground_types = {row["ユニット名"] for row in csv.DictReader(source)
                        if row["カテゴリ"] == "地上部隊"}
    with (master / "unit.csv").open() as source:
        combat_types = {row["名前"] for row in csv.DictReader(source)
                        if row["名前"] in ground_types and (row["武器1"] or row["武器2"])}
    player = snapshots[0]["player_id"]
    # 生産可能範囲はゲームルールの3。勝敗や戦力倍率を達成の代用にしない。
    first_order = first_entry = first_block = first_sustained_block = None
    fire_follow = []
    rows = []
    produced = Counter()
    for index, snapshot in enumerate(snapshots):
        turn = snapshot["before"]["turn"]
        routes = snapshot.get("routes", {}).get("routes", [])
        before = {unit["unit_id"]: unit for unit in snapshot["before"]["units"]}
        after = {unit["unit_id"]: unit for unit in snapshot["after"]["units"]}
        last_attacks = {}
        for event in snapshot["events"]:
            if event["type"] == "unit_produced" and event.get("player_id") == player:
                produced[event["unit_type"]] += 1
            if event["type"] == "unit_attacked" and event.get("attacker_player_id") == player:
                last_attacks[event["defender_id"]] = event
        for defender, event in last_attacks.items():
            attacker = before.get(event["attacker_id"])
            enemy = before.get(defender)
            if defender in after or not attacker or not enemy:
                continue
            if attacker["unit_type"] not in {"砲台", "軽自走砲", "重自走砲", "ロケットランチャー"}:
                continue
            for unit in after.values():
                previous = before.get(unit["unit_id"])
                if (unit["player_id"] == player and previous
                        and unit["unit_type"] in {"軽戦車", "戦車", "中戦車", "重戦車", "軽歩兵", "重歩兵"}
                        and not unit["transporting_by"]
                        and position(previous) != position(unit) == position(enemy)):
                    # 敵がいたセルへの進入なので、砲撃より前の移動を誤計上しない。
                    fire_follow.append({"turn": turn, "gun": attacker["unit_type"],
                                        "gun_at": position(attacker), "removed": enemy["unit_type"],
                                        "tile": position(enemy), "follower": unit["unit_type"],
                                        "from": position(previous), "hp_after": unit["hp"]})
        for route in routes:
            goal = route["goal"]
            units = route["ground_units"]
            # 配送任務などでDAG外にいる部隊も、盤面上の地上戦闘種なら数える。
            combat = [unit for unit in after.values() if unit["unit_type"] in combat_types
                      and unit["player_id"] == player and not unit["transporting_by"]]
            entry = [unit for unit in combat if hex_distance(unit, goal) <= 3]
            orders = sum(unit.get("target_anchor") == goal and unit.get("phase") == "Advance"
                         for unit in units)
            destinations = sum(unit.get("target_x") == goal["x"]
                               and unit.get("target_y") == goal["y"]
                               and unit.get("phase") == "Advance" for unit in units)
            factories = [site for site in snapshots[0]["before"]["properties"]
                         if site["terrain_type"] == "Factory" and site["owner"] not in (None, player)
                         and hex_distance(site, goal) <= 3]

            def blocked(board, site):
                owned = any(position(prop) == position(site) and prop["owner"] == player
                            for prop in board["properties"])
                occupied = any(position(unit) == position(site) and unit["player_id"] == player
                               and not unit["transporting_by"] for unit in board["units"])
                return owned or occupied

            blocks = [position(site) for site in factories if blocked(snapshot["after"], site)]
            # 次の自手番開始盤面でも塞いでいることを別に確認する。
            sustained = [] if index + 1 == len(snapshots) else [
                position(site) for site in factories
                if blocked(snapshot["after"], site) and blocked(snapshots[index + 1]["before"], site)]
            if orders and first_order is None:
                first_order = turn
            if entry and first_entry is None:
                first_entry = {"turn": turn, "units": [
                    {"type": unit["unit_type"], "at": position(unit)} for unit in entry]}
            if blocks and first_block is None:
                first_block = {"turn": turn, "sites": blocks}
            if sustained and first_sustained_block is None:
                first_sustained_block = {"turn": turn, "sites": sustained}
            if turn % 10 == 0 or index + 1 == len(snapshots):
                open_factories = [position(site) for site in snapshot["after"]["properties"]
                                  if site["terrain_type"] == "Factory" and site["owner"] not in (None, player)
                                  and hex_distance(site, goal) <= 3 and not blocked(snapshot["after"], site)]
                rows.append({"turn": turn, "capital_orders": orders, "capital_destinations": destinations,
                             "nearest_combat": min((hex_distance(unit, goal) for unit in combat), default=None),
                             "production_zone_units": len(entry), "blocked_factories": blocks,
                             "sustained_blocks": sustained, "open_enemy_factories": open_factories,
                             "owned_properties": sum(site["owner"] == player
                                                     for site in snapshot["after"]["properties"])})
    return {"file": str(path), "through": snapshots[-1]["before"]["turn"],
            "first_capital_order": first_order, "first_combat_entry": first_entry,
            "first_factory_block": first_block, "first_sustained_factory_block": first_sustained_block,
            "produced_units": dict(produced), "fire_then_fill_count": len(fire_follow),
            "fire_then_fill_examples": fire_follow if len(fire_follow) <= 12 else fire_follow[:6] + fire_follow[-6:],
            "rows": rows}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("records", type=Path, nargs="+")
    args = parser.parse_args()
    for record in args.records:
        print(json.dumps(review(record), ensure_ascii=False))
