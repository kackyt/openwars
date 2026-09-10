#!/usr/bin/env python3
"""同条件のseed別対局を比較する。欠損や上限判定を通常の敗北・勝利へ混ぜない。"""

import argparse
import json
from collections import Counter
from pathlib import Path
from statistics import mean


def load_seed(directory, seed):
    path = Path(directory) / f"seed{seed}.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, ValueError) as error:
        raise ValueError(f"{path}: {error}") from error
    games = data.get("results", [])
    if len(games) != 1:
        raise ValueError(f"{path}: results must contain exactly one game")
    game = games[0]
    if game.get("seed") != seed:
        raise ValueError(f"{path}: seed does not match the filename")
    if game.get("error"):
        raise ValueError(f"{path}: game error: {game['error']}")
    metadata = data.get("metadata", {})
    if not isinstance(metadata.get("max_turns"), int) or metadata["max_turns"] <= 0:
        raise ValueError(f"{path}: metadata.max_turns is required")
    for key in ("map", "grid_type", "p1", "p2"):
        if not isinstance(game.get(key), str) or not game[key]:
            raise ValueError(f"{path}: game.{key} is required")
    if not isinstance(game.get("turns"), int) or game["turns"] < 0:
        raise ValueError(f"{path}: game.turns is required")
    return game, metadata


def resolve_side(game, subject, explicit_side=None):
    # 同版同士や版名の不一致を、暗黙のP2に読み替えない。
    matching = [side for side in (1, 2) if game[f"p{side}"].upper() == subject.upper()]
    if explicit_side is not None:
        if explicit_side not in matching:
            raise ValueError(f"P{explicit_side} is not {subject}")
        return explicit_side
    if len(matching) != 1:
        raise ValueError(f"cannot identify {subject}; specify --side for a same-version match")
    return matching[0]


def outcome(game, side):
    result = game.get("result")
    if result in ("Draw", "Draw_MaxTurns"):
        return "draw" if result == "Draw" else "limit_draw"
    for winner in (1, 2):
        if result == f"P{winner}_Win":
            return "win" if winner == side else "loss"
        if result == f"P{winner}_Win_MaxTurns":
            return "limit_win" if winner == side else "limit_loss"
    raise ValueError(f"unknown result: {result!r}")


def comparison_key(game, metadata, side):
    # 評価対象の版は変更できるが、相手・手番・盤面・上限は固定する。
    return (
        game["map"], game["grid_type"], side,
        game[f"p{3 - side}"].upper(), metadata["max_turns"],
        metadata.get("criteria"),
    )


def load_pairs(baseline, candidate, seeds, subject, baseline_subject=None, side=None):
    pairs = []
    expected = None
    for seed in seeds:
        base, base_meta = load_seed(baseline, seed)
        new, new_meta = load_seed(candidate, seed)
        base_side = resolve_side(base, baseline_subject or subject, side)
        new_side = resolve_side(new, subject, side)
        base_key = comparison_key(base, base_meta, base_side)
        new_key = comparison_key(new, new_meta, new_side)
        if base_key != new_key or (expected is not None and base_key != expected):
            raise ValueError(f"seed {seed}: matchup conditions differ: {base_key} / {new_key}")
        expected = base_key
        outcome(base, base_side)
        outcome(new, new_side)
        pairs.append((seed, base, new, new_side))
    if not pairs:
        raise ValueError("at least one seed is required")
    return pairs


def objective_at(game, side, turn=None):
    metrics = game.get("metrics", [])
    # 未観測のターンへ終局状態を持ち越さない。
    snapshots = metrics if turn is None else [m for m in metrics if m.get("turn") == turn]
    return snapshots[-1].get(f"p{side}_obj", {}) if snapshots else {}


def display(value):
    return "N/A" if value is None else f"{value:g}" if isinstance(value, float) else str(value)


def game_summary(game, side, turn):
    obj = objective_at(game, side, turn)
    return (
        f"{game['result']} / T{game['turns']} / "
        f"ZOC {display(obj.get('zoc_area'))} / "
        f"拠点 {display(obj.get('owned_properties'))} / "
        f"収入 {display(obj.get('income_per_turn'))}"
    )


def aggregate(games, side):
    counts = Counter(outcome(game, side) for game in games)
    samples = []
    production = Counter()
    missing_thinking = 0
    missing_production = 0
    for game in games:
        timings = game.get("thinking_times", {}).get(str(side))
        if timings:
            samples.extend(timings)
        else:
            missing_thinking += 1
        produced = game.get("action_counts", {}).get(str(side))
        if produced is None:
            missing_production += 1
        else:
            production.update(produced)
    return {
        "outcomes": counts,
        "turns": sum(game["turns"] for game in games),
        "thinking_mean": mean(samples) if samples else None,
        "thinking_samples": len(samples),
        "missing_thinking": missing_thinking,
        "production": production,
        "missing_production": missing_production,
    }


def render(pairs, baseline, candidate, turn=None):
    side = pairs[0][3]
    base = aggregate([pair[1] for pair in pairs], side)
    new = aggregate([pair[2] for pair in pairs], side)
    n = len(pairs)
    lines = [
        "# AI対戦比較",
        f"基準: {baseline}",
        f"候補: {candidate}",
        f"対象: P{side} / {pairs[0][1][f'p{side}']} → {pairs[0][2][f'p{side}']} / {n}対局",
        "",
        "| 結果 | 基準 | 候補 |",
        "|---|---:|---:|",
    ]
    for key, label in (
        ("win", "通常勝利"), ("loss", "通常敗北"),
        ("limit_win", "上限判定勝利"), ("limit_loss", "上限判定敗北"),
        ("draw", "通常引分"), ("limit_draw", "上限引分"),
    ):
        lines.append(f"| {label} | {base['outcomes'][key]} | {new['outcomes'][key]} |")
    lines.extend([
        "",
        f"通常勝利/全対局: {base['outcomes']['win']}/{n} → {new['outcomes']['win']}/{n}",
        f"総対局ターン: {base['turns']} → {new['turns']}",
        f"思考時間の標本平均(ms): {display(base['thinking_mean'])} → {display(new['thinking_mean'])}",
        f"思考時間の標本数: {base['thinking_samples']} → {new['thinking_samples']} "
        f"（未記録対局: {base['missing_thinking']} → {new['missing_thinking']}）",
        "",
        f"指標の観測時点: {'終局時' if turn is None else f'T{turn}の実測値'}。N/Aは未観測。",
        "終局時の数値だけから改善・悪化を判定しない。同時要件は別途検証する。",
        "",
        "| Seed | 基準（結果 / 終局 / 指標） | 候補（結果 / 終局 / 指標） | 通常勝利の変化 |",
        "|---|---|---|---|",
    ])
    for seed, b, t, side in pairs:
        b_win, t_win = outcome(b, side) == "win", outcome(t, side) == "win"
        tag = "通常勝利へ改善" if t_win and not b_win else (
            "通常勝利を失う退行" if b_win and not t_win else "—"
        )
        lines.append(
            f"| {seed} | {game_summary(b, side, turn)} | {game_summary(t, side, turn)} | {tag} |"
        )
    lines.extend(["", "生産総数は対局長と合わせて読む。"])
    if base["missing_production"] or new["missing_production"]:
        lines.append(
            f"生産内訳に未記録対局あり（基準 {base['missing_production']} / "
            f"候補 {new['missing_production']}）。総数の比較を省略。"
        )
    else:
        lines.extend(["", "| 兵種 | 基準 | 候補 |", "|---|---:|---:|"])
        for kind in sorted(base["production"].keys() | new["production"].keys()):
            lines.append(f"| {kind} | {base['production'][kind]} | {new['production'][kind]} |")
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline_dir", type=Path)
    parser.add_argument("target_dir", type=Path)
    parser.add_argument("--subject", default="V4")
    parser.add_argument("--baseline-subject", help="基準側の評価対象版。省略時は--subjectと同じ")
    parser.add_argument("--side", type=int, choices=(1, 2))
    parser.add_argument("--seeds", type=int, default=12, help="seed 1からこの数まで比較")
    parser.add_argument("--seed-list", type=int, nargs="+", help="比較するseedを明示（--seedsより優先）")
    parser.add_argument("--turn", type=int, help="このターンの実測指標を比較。省略時は終局時")
    parser.add_argument("--output", type=Path, help="新規UTF-8レポート。既存ファイルは上書きしない")
    args = parser.parse_args(argv)
    seeds = args.seed_list if args.seed_list is not None else list(range(1, args.seeds + 1))
    if not seeds or len(set(seeds)) != len(seeds) or any(seed < 0 for seed in seeds):
        parser.error("provide a nonempty list of distinct nonnegative seeds")
    if args.turn is not None and args.turn < 0:
        parser.error("--turn must be nonnegative")
    try:
        pairs = load_pairs(
            args.baseline_dir, args.target_dir, seeds,
            args.subject, args.baseline_subject, args.side,
        )
        report = render(pairs, args.baseline_dir, args.target_dir, args.turn)
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            with args.output.open("x", encoding="utf-8", newline="\n") as output:
                output.write(report)
        else:
            print(report, end="")
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
