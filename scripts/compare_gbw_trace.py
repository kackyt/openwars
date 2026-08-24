#!/usr/bin/env python3
"""GB版命令CSVとOpenWars JSONLの短期行動一致率を比較する。"""

from __future__ import annotations

import argparse
import csv
import json
import re
from collections import Counter, defaultdict
from pathlib import Path


GB_ACTIONS = {
    "6": "Capture",
    "7": "Attack",
    "8": "Drop",
    "9": "Wait",
    "13": "Load",
}
OW_ACTION = re.compile(r"^([A-Za-z]+)")
OW_POSITION = re.compile(
    r"(?:target_pos|transport_target_pos): GridPosition \{ x: (\d+), y: (\d+)"
)


def read_gb_actions(path: Path, max_turn: int) -> dict[tuple[int, int], list[str]]:
    """GB CSVの連続する陣営別行動ブロックを2ターン目以降へ割り当てる。"""
    grouped: dict[tuple[int, int], list[str]] = defaultdict(list)
    turn_by_side = {"0": 1, "1": 1}
    last_side: str | None = None
    with path.open(encoding="utf-8", newline="") as source:
        for row in csv.DictReader(source):
            action = GB_ACTIONS.get(row["command"])
            if action is None:
                continue
            side = row["side"]
            if side != last_side:
                turn_by_side[side] += 1
                last_side = side
            turn = turn_by_side[side]
            if turn > max_turn:
                continue
            player = int(side) + 1
            # GBは1始まり、OpenWarsは0始まりなので各軸を1引く。
            x = int(row["target_x"]) - 1
            y = int(row["target_y"]) - 1
            grouped[(turn, player)].append(f"{action}:{x},{y}")
    return grouped


def read_openwars_actions(path: Path, max_turn: int) -> dict[tuple[int, int], list[str]]:
    grouped: dict[tuple[int, int], list[str]] = defaultdict(list)
    with path.open(encoding="utf-8") as source:
        for line in source:
            row = json.loads(line)
            turn = int(row["turn"])
            if turn > max_turn:
                continue
            player = int(row["player_id"])
            for raw_action in row["actions"]:
                if raw_action.startswith("ProduceUnitCommand"):
                    continue
                action_match = OW_ACTION.search(raw_action)
                position_match = OW_POSITION.search(raw_action)
                if action_match is None or position_match is None:
                    continue
                action = action_match.group(1)
                x, y = position_match.groups()
                grouped[(turn, player)].append(f"{action}:{x},{y}")
    return grouped


def compare_group(expected: list[str], actual: list[str]) -> tuple[int, int]:
    """順不同の多重集合一致数と、同じ添字での順序込み一致数を返す。"""
    multiset_matches = sum((Counter(expected) & Counter(actual)).values())
    ordered_matches = sum(left == right for left, right in zip(expected, actual))
    return multiset_matches, ordered_matches


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gb", type=Path, required=True, help="GBトレーサーのCSV")
    parser.add_argument(
        "--openwars", type=Path, required=True, help="eval_matchup.pyのJSONL"
    )
    parser.add_argument("--max-turn", type=int, default=5)
    args = parser.parse_args()

    expected = read_gb_actions(args.gb, args.max_turn)
    actual = read_openwars_actions(args.openwars, args.max_turn)
    total_expected = 0
    total_actual = 0
    total_multiset = 0
    total_ordered = 0

    print("| Turn | Player | GB | OpenWars | Action+target | Ordered |")
    print("|---:|---:|---:|---:|---:|---:|")
    for key in sorted(expected.keys() | actual.keys()):
        gb_actions = expected.get(key, [])
        ow_actions = actual.get(key, [])
        multiset_matches, ordered_matches = compare_group(gb_actions, ow_actions)
        total_expected += len(gb_actions)
        total_actual += len(ow_actions)
        total_multiset += multiset_matches
        total_ordered += ordered_matches
        turn, player = key
        print(
            f"| {turn} | {player} | {len(gb_actions)} | {len(ow_actions)} | "
            f"{multiset_matches}/{len(gb_actions)} | "
            f"{ordered_matches}/{len(gb_actions)} |"
        )

    multiset_rate = 100.0 * total_multiset / total_expected if total_expected else 0.0
    ordered_rate = 100.0 * total_ordered / total_expected if total_expected else 0.0
    print()
    print(
        f"Total: GB={total_expected}, OpenWars={total_actual}, "
        f"action+target={total_multiset}/{total_expected} ({multiset_rate:.1f}%), "
        f"ordered={total_ordered}/{total_expected} ({ordered_rate:.1f}%)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
