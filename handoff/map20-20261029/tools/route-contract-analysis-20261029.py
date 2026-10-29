# -*- coding: utf-8 -*-
"""P2 進軍路の切替・移動・西拠点所有を、同一個体・連続ラウンドで集計する検証スクリプト。

使い方 (例):
  python -I route-contract-analysis-20261029.py <seedN.json> [...]

集計方針:
- 分母は「ラウンド t と t+1 の両方で capital_route_history に現れ、かつ両方で route_index が None でない同一 entity」のみ。
  出生・消滅・欠測・未コミット (route_index None) はすべて分母から除外する。
- 切替は分母内で route_index が変わったものを数える。
- 同じ遷移 (t -> t+1) で西拠点 (6,17), (2,18), (2,19), (10,18) の所有が変わったかで切替を分ける。
- 移動は strategic_history.units の round 末座標で、t と t+1 の両方に存在する entity のみ数える。
- 検査: ラウンド重複, round!=turn, 欠測ラウンド, 同一ラウンド内の entity 重複, route_index None の件数。
"""
import argparse
import json

# マップ20の西側で失われる拠点。記録IDではなく盤面座標で照合する。
WEST = [(6, 17), (2, 18), (2, 19), (10, 18)]


def run(path, until):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    games = data.get("results", [])
    if len(games) != 1 or games[0].get("error"):
        raise ValueError(f"対局の欠損・エラー: {path}")
    r = games[0]
    if (r.get("map"), r.get("p1"), r.get("p2"), r.get("grid_type")) != (
        "map_20", "V200", "V4", "hex"
    ):
        raise ValueError(f"対象条件が異なる: {path}")
    cap = [x for x in r["capital_route_history"] if x["player_id"] == 2]
    rounds = [x["round"] for x in cap]
    if not rounds or len(rounds) != len(set(rounds)):
        raise ValueError(f"経路記録の欠損・重複: {path}")
    rep = {"file": path}
    rep["checks"] = {
        "rounds_n": len(rounds),
        "dup_rounds": len(rounds) - len(set(rounds)),
        "round_ne_turn": sum(1 for x in cap if x["round"] != x["turn"]),
        "missing_rounds": sorted(set(range(1, r["turns"] + 1)) - set(rounds)),
    }
    rt = {}
    details = {}
    dup_ent = 0
    idx_none = 0
    for x in cap:
        m = {}
        detail = {}
        for route in x["routes"]["routes"]:
            for g in route["ground_units"]:
                e = g["entity_id"]
                if e in m:
                    raise ValueError(f"同一ラウンドに重複するEntity: {path} {x['round']} {e}")
                m[e] = g["route_index"]
                detail[e] = g
                if g["route_index"] is None:
                    idx_none += 1
        rt[x["round"]] = m
        details[x["round"]] = detail
    rep["checks"]["dup_entity_in_round"] = dup_ent
    rep["checks"]["route_index_none"] = idx_none

    S = {s["turn"]: s for s in r["strategic_history"]}
    if len(S) != len(r["strategic_history"]):
        raise ValueError(f"盤面記録のラウンド重複: {path}")

    def owners(t):
        return {(p["x"], p["y"]): p["owner"] for p in S[t]["properties"]}

    def pos(t, e):
        for u in S[t]["units"]:
            if u["player_id"] == 2 and u["unit_id"] == e:
                return (u["x"], u["y"])
        return None

    pairs = switches = sw_west_change = sw_no_change = sw_no_property_change = 0
    mv_den = mv_moved = 0
    forward = {"valid_pairs": 0, "advanced": 0, "unchanged": 0, "retreated": 0}
    resolved = {"resolved": 0, "unresolved": 0, "unknown": 0}
    for turn, entries in details.items():
        if until is not None and turn > until:
            continue
        for unit in entries.values():
            if unit.get("route_index") is None:
                continue
            value = unit.get("order_resolved")
            resolved["resolved" if value is True else "unresolved" if value is False else "unknown"] += 1
    observed = [
        route for turn, units in rt.items()
        if until is None or turn <= until
        for route in units.values()
    ]
    rep["route_coverage_in_horizon"] = {
        "committed": sum(route is not None for route in observed),
        "uncommitted": sum(route is None for route in observed),
    }
    for t in sorted(rt):
        if until is not None and t + 1 > until:
            continue
        if t + 1 not in rt or t not in S or t + 1 not in S:
            continue
        ot, ot1 = owners(t), owners(t + 1)
        west_changed = any(ot.get(p) != ot1.get(p) for p in WEST)
        for e, idx in rt[t].items():
            idx2 = rt[t + 1].get(e, None) if e in rt[t + 1] else "absent"
            if idx is None or idx2 is None or idx2 == "absent":
                continue
            pairs += 1
            if idx != idx2:
                switches += 1
                if west_changed:
                    sw_west_change += 1
                else:
                    sw_no_change += 1
                if ot == ot1:
                    sw_no_property_change += 1
            before, after = details[t][e], details[t + 1][e]
            old_progress, new_progress = before.get("progress"), after.get("progress")
            if (
                idx == idx2
                and after.get("phase") == "Advance"
                and after.get("order_resolved") is True
                and isinstance(old_progress, int)
                and isinstance(new_progress, int)
            ):
                forward["valid_pairs"] += 1
                key = "advanced" if new_progress > old_progress else "retreated" if new_progress < old_progress else "unchanged"
                forward[key] += 1
            p1, p2 = pos(t, e), pos(t + 1, e)
            if p1 and p2:
                mv_den += 1
                if p1 != p2:
                    mv_moved += 1
    rep["route_switch"] = {
        "valid_pairs": pairs,
        "switches": switches,
        "switch_on_west_owner_change_same_step": sw_west_change,
        "switch_no_west_owner_change": sw_no_change,
        "switch_no_property_owner_change": sw_no_property_change,
    }
    rep["resolved_committed_orders"] = resolved
    rep["same_route_advance_progress"] = forward
    rep["moved"] = {"den": mv_den, "moved": mv_moved}
    rep["west_owner_6_17__2_18__2_19__10_18"] = {
        f"T{t}": [owners(t).get(p) for p in WEST] for t in (7, 10, 15) if t in S
    }
    return rep


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--until", type=int)
    parser.add_argument("paths", nargs="+")
    args = parser.parse_args()
    if args.until is not None and args.until < 2:
        parser.error("--until は2以上を指定してください")
    for path in args.paths:
        print(json.dumps(run(path, args.until), ensure_ascii=False))
