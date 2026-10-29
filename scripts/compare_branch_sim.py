"""分岐シミュレーション（`run_branch_sim.py`）の結果を、実際の対局ログや別のシミュレーションと比べる。

入力はどれも `seed*.json` を含むディレクトリで、各ファイルの `results[0].strategic_history`
（ラウンドごとの終わりの盤面）を読む。対局スイープの条件ディレクトリも、分岐シミュレーションの
出力ディレクトリも同じように渡せる。指定ラウンドの指標を局ごとに数え、ディレクトリ別の平均を並べる。

使用例:

    python scripts/compare_branch_sim.py --rounds 13,17 \\
        actual=.tmp/sweep/games/current/map_20_V200-V4_r0 \\
        before=.tmp/branch/before after=.tmp/branch/after \\
        --region west:0-12,* --region home:15-24,14-* \\
        --cities west_cities:10,18;6,17;2,18;2,19

指標:

- `pN_props` … プレイヤーNの拠点数
- `pN_value` … プレイヤーNの戦力（部隊の価格×HP%の合計）
- `pN_<領域>` … `--region` で指定した領域内のプレイヤーNの戦力
- `<名前>` … `--cities` で指定した拠点のうち `--player` が持つ数
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path


def parse_range(text: str) -> tuple[int | None, int | None]:
    """`a-b` / `a-` / `-b` / `a` / `*` を (下限, 上限) にする（None は制限なし）。"""
    if text in ("*", ""):
        return None, None
    low, sep, high = text.partition("-")
    if not sep:
        return int(low), int(low)
    return (int(low) if low else None), (int(high) if high else None)


def parse_region(spec: str) -> tuple[str, tuple, tuple]:
    """`名前:x範囲,y範囲` を (名前, x範囲, y範囲) にする。"""
    name, sep, body = spec.partition(":")
    xs, comma, ys = body.partition(",")
    if not sep or not comma:
        raise SystemExit(f"--region は 名前:x範囲,y範囲 の形で指定する: {spec}")
    return name, parse_range(xs), parse_range(ys)


def parse_cities(spec: str) -> tuple[str, set[tuple[int, int]]]:
    """`名前:x,y;x,y` を (名前, 座標の集合) にする。"""
    name, sep, body = spec.partition(":")
    if not sep:
        raise SystemExit(f"--cities は 名前:x,y;x,y の形で指定する: {spec}")
    cells = set()
    for cell in body.split(";"):
        x, y = cell.split(",")
        cells.add((int(x), int(y)))
    return name, cells


def within(value: int, bounds: tuple[int | None, int | None]) -> bool:
    low, high = bounds
    return (low is None or value >= low) and (high is None or value <= high)


def board_metrics(board: dict, regions: list, cities: list, player: int) -> Counter:
    """1ラウンドの盤面から指標を数える。輸送中の部隊は数えない。"""
    metrics: Counter = Counter()
    # 部隊・拠点が0の局も平均に入るよう、盤面に出てくるプレイヤーの指標を0で用意しておく
    players = {unit["player_id"] for unit in board["units"]}
    players |= {prop["owner"] for prop in board["properties"] if prop["owner"]}
    for owner in players:
        metrics[f"p{owner}_value"] = 0
        metrics[f"p{owner}_props"] = 0
        for name, _, _ in regions:
            metrics[f"p{owner}_{name}"] = 0
    for unit in board["units"]:
        if unit.get("transporting_by") is not None:
            continue
        owner = unit["player_id"]
        value = unit["cost"] * unit["hp"] // 100
        metrics[f"p{owner}_value"] += value
        for name, xs, ys in regions:
            if within(unit["x"], xs) and within(unit["y"], ys):
                metrics[f"p{owner}_{name}"] += value
    for prop in board["properties"]:
        if prop["owner"]:
            metrics[f"p{prop['owner']}_props"] += 1
    for name, cells in cities:
        metrics[name] = sum(
            1
            for prop in board["properties"]
            if (prop["x"], prop["y"]) in cells and prop["owner"] == player
        )
    return metrics


def load(
    directory: Path, rounds: set[int], regions: list, cities: list, player: int
) -> dict[int, dict[str, list[int]]]:
    """ディレクトリ内の全局について、ラウンド別・指標別の値の並びを返す。"""
    data: dict[int, dict[str, list[int]]] = defaultdict(lambda: defaultdict(list))
    files = sorted(directory.glob("seed*.json"))
    if not files:
        raise SystemExit(f"seed*.json が見つからない: {directory}")
    for path in files:
        with open(path, encoding="utf-8") as stream:
            result = json.load(stream)["results"][0]
        for board in result.get("strategic_history", []):
            if board["turn"] in rounds:
                for key, value in board_metrics(board, regions, cities, player).items():
                    data[board["turn"]][key].append(value)
    return data


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dirs", nargs="+", metavar="ラベル=ディレクトリ")
    parser.add_argument(
        "--rounds", required=True, help="比べるラウンド（カンマ区切り）"
    )
    parser.add_argument(
        "--region", action="append", default=[], help="名前:x範囲,y範囲（繰り返し可）"
    )
    parser.add_argument(
        "--cities", action="append", default=[], help="名前:x,y;x,y（繰り返し可）"
    )
    parser.add_argument(
        "--player", type=int, default=2, help="--cities を数える側のプレイヤー"
    )
    args = parser.parse_args()

    rounds = [int(text) for text in args.rounds.split(",")]
    regions = [parse_region(spec) for spec in args.region]
    cities = [parse_cities(spec) for spec in args.cities]
    labels = []
    for item in args.dirs:
        label, sep, directory = item.partition("=")
        if not sep:
            raise SystemExit(f"ラベル=ディレクトリ の形で指定する: {item}")
        labels.append((label, Path(directory)))
    data = {
        label: load(directory, set(rounds), regions, cities, args.player)
        for label, directory in labels
    }

    for turn in rounds:
        keys = sorted({key for label, _ in labels for key in data[label][turn]})
        counts = ", ".join(
            f"{label}={len(data[label][turn].get('p1_value', []))}局"
            for label, _ in labels
        )
        print(f"--- ラウンド{turn}終了時の平均（{counts}。戦力は千単位）")
        for key in keys:
            cells = []
            for label, _ in labels:
                values = data[label][turn].get(key, [])
                mean = statistics.mean(values) if values else float("nan")
                is_value = key.startswith("p") and not key.endswith("_props")
                is_value = is_value and key not in {name for name, _ in cities}
                cells.append(
                    f"{label}={mean / 1000:.0f}k" if is_value else f"{label}={mean:.1f}"
                )
            print(f"  {key:14s} " + "  ".join(cells))
    return 0


if __name__ == "__main__":
    sys.exit(main())
