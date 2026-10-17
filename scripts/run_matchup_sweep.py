"""複数のAI変種（バイナリ・環境変数）× マップ × seed × 先後 × 繰返しを並列に対戦させて比較する。

変種はサーバー起動時に決まる設定（mcp-serverのバイナリ、診断出力などの
環境変数）の組で、1本のサーバーの中では切り替えられない。本スクリプトは全変種の対局を
1本の作業キューにまとめ、全体の並列数を `--jobs` だけで制御する。

使用例（基準版と現在版を map_6/map_25 の seed 1,2 で各3回ずつ比較）:

    python scripts/run_matchup_sweep.py \\
        --variant baseline=.tmp/bench/mcp-server-baseline.exe \\
        --variant current=.tmp/bench/mcp-server-current.exe \\
        --map map_6,map_25 --seeds 1,2 --repeats 3 --jobs 8 \\
        --out-dir .tmp/sweep-YYYYMMDD

環境変数つきの変種:

    --variant trace=.tmp/bench/mcp-server-current.exe \\
    --env trace:OPENWARS_TRACE_AI_STEPS=1

出力（`--out-dir` 配下）:

- `games/<変種>/<map>_<P1>-<P2>_r<繰返し>/seed<seed>.json` … 1局ごとの生データ。
  完了するたびに書くので、途中で止めても終わった対局は残る。形式は
  `eval_matchup.py --json-output` と同じ `{"metadata": ..., "results": [1局]}` で、
  条件ディレクトリ1つが `compare_benchmarks.py` の入力ディレクトリにそのまま使える。
  `--games` が2以上のときは条件ディレクトリ名に `_g<局番号>` が付く。
- `logs/<変種>/...log` … 1局ごとの詳細ログ（従来の [LOG] 行とメトリクス）。
- `logs/servers/` … `OPENWARS_TRACE_AI_STEPS` 使用時のサーバーごとのstderr。
- `<変種>.md` … 変種ごとの集計レポート（`eval_matchup.py` と同じ形式）。
- `summary.md` / `summary.json` … 同じ対局条件ごとに、変種別の勝敗・ターン数の分布と
  行動列（action_history）ハッシュの集合を並べた比較表。

標準出力には完了ごとの進捗1行だけを出す。終了時（正常・例外・Ctrl+C）には、この
プロセスが起動したmcp-serverだけを終了させる。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from collections import Counter, defaultdict
from pathlib import Path

if __package__:
    from scripts import matchup_runner
    from scripts.eval_issue58 import parse_seed_list, write_json_atomic
    from scripts.eval_matchup import build_match_specs, generate_report, run_single_game
else:
    # 直接実行時は別 checkout の scripts パッケージではなく同階層を読む。
    import matchup_runner
    from eval_issue58 import parse_seed_list, write_json_atomic
    from eval_matchup import build_match_specs, generate_report, run_single_game


def parse_variants(raw_variants: list[str], raw_envs: list[str]) -> dict[str, dict]:
    """`--variant NAME=BIN` と `--env NAME:KEY=VALUE` から変種の定義を作る。"""
    variants: dict[str, dict] = {}
    for raw in raw_variants:
        name, sep, binary = raw.partition("=")
        if not sep or not name or not binary:
            raise ValueError(f"--variant must be NAME=BIN: {raw}")
        if name in variants:
            raise ValueError(f"duplicate variant: {name}")
        if not Path(binary).is_file():
            raise ValueError(f"mcp-server binary not found for {name}: {binary}")
        variants[name] = {"bin": os.path.abspath(binary), "env": {}}
    for raw in raw_envs:
        name, sep, assignment = raw.partition(":")
        key, eq, value = assignment.partition("=")
        if not sep or not eq or not key:
            raise ValueError(f"--env must be NAME:KEY=VALUE: {raw}")
        if name not in variants:
            raise ValueError(f"--env refers to unknown variant: {name}")
        variants[name]["env"][key] = value
    if not variants:
        raise ValueError("at least one --variant is required")
    return variants


def action_hash(result: dict) -> str:
    """行動列の同一性を比べるための短いハッシュ。"""
    payload = json.dumps(result.get("action_history", []), sort_keys=True)
    return hashlib.md5(payload.encode("utf-8")).hexdigest()[:8]


def build_jobs(variants: dict, specs: list[dict], repeats: int) -> list[dict]:
    """変種ごとにまとめて並べ、各スレッドがサーバーを作り直す回数を減らす。

    同じ条件・seedの対局が複数ある（--games 2以上）場合は、出現順の局番号を付ける。
    """
    occurrence: Counter = Counter()
    numbered = []
    for spec in specs:
        key = (spec["map"], spec["p1"], spec["p2"], spec["seed"])
        numbered.append(dict(spec, game=occurrence[key]))
        occurrence[key] += 1
    jobs = []
    for variant in variants:
        for repeat in range(repeats):
            for spec in numbered:
                jobs.append(dict(spec, variant=variant, repeat=repeat))
    return jobs


def condition_dir(job: dict, games_per_seed: int) -> str:
    """compare_benchmarks.py の入力にできる、同一条件の対局をまとめるディレクトリ名。"""
    name = f"{job['map']}_{job['p1']}-{job['p2']}_r{job['repeat']}"
    if games_per_seed > 1:
        name += f"_g{job['game']}"
    return name


def summarize(rows: list[dict], variant_names: list[str]) -> list[dict]:
    """同じ対局条件ごとに、変種別の結果分布と行動列ハッシュの集合をまとめる。"""
    grouped: dict[tuple, dict[str, list[dict]]] = defaultdict(lambda: defaultdict(list))
    for row in rows:
        key = (row["map"], row["seed"], row["p1"], row["p2"])
        grouped[key][row["variant"]].append(row)
    summary = []
    for key in sorted(grouped, key=lambda k: (k[0], str(k[1]), k[2], k[3])):
        entry = {"map": key[0], "seed": key[1], "p1": key[2], "p2": key[3], "variants": {}}
        for name in variant_names:
            games = sorted(grouped[key].get(name, []), key=lambda r: r["repeat"])
            entry["variants"][name] = {
                "outcomes": dict(Counter(f"{g['result']} {g['turns']}T" for g in games)),
                "action_hashes": dict(Counter(g["action_hash"] for g in games)),
                "errors": [g["error"] for g in games if g.get("error")],
            }
        hash_sets = [set(v["action_hashes"]) for v in entry["variants"].values()]
        # 行動列の集合が全変種で同じなら、少なくともこの繰返し回数では挙動差なし。
        entry["same_action_set"] = all(h == hash_sets[0] for h in hash_sets)
        # 集合が違っても共通の行動列があれば、実行ごとの揺れを繰返し回数が拾いきれて
        # いない可能性がある。共通が1つもなければ挙動差の疑いが強い。
        entry["verdict"] = (
            "same"
            if entry["same_action_set"]
            else "overlap"
            if set.intersection(*hash_sets)
            else "disjoint"
        )
        summary.append(entry)
    return summary


def write_summary_markdown(path: Path, summary: list[dict], variant_names: list[str], repeats: int) -> None:
    lines = [
        "# 対戦スイープ比較",
        "",
        f"変種: {', '.join(variant_names)} ／ 繰返し: {repeats}",
        "",
        "判定は各変種で出現した action_history ハッシュの集合を比べたもの。",
        "通常経路のマップは同じバイナリでも実行ごとに行動列が分かれることがあるため、",
        "1回ずつの比較ではなく分布（集合と出現回数）で判断する。",
        "",
        "- ✅ 集合が一致：この繰返し回数では挙動差なし。",
        "- 🟡 共通あり：集合は違うが共通の行動列がある。実行ごとの揺れを拾いきれていない可能性が",
        "  高いので、--repeats を増やして再確認する。これだけで退行と判断しない。",
        "- ❌ 共通なし：どの回も別の行動列。挙動差の疑いが強いので、分岐した手を調べる。",
        "",
        "| マップ | seed | P1 | P2 | " + " | ".join(variant_names) + " | 判定 |",
        "|---|---|---|---|" + "---|" * len(variant_names) + "---|",
    ]
    for entry in summary:
        cells = []
        for name in variant_names:
            data = entry["variants"][name]
            outcomes = ", ".join(f"{k}×{v}" for k, v in sorted(data["outcomes"].items()))
            hashes = ", ".join(f"{k}×{v}" for k, v in sorted(data["action_hashes"].items()))
            error = f" ⚠ error×{len(data['errors'])}" if data["errors"] else ""
            cells.append(f"{outcomes}<br>`{hashes}`{error}")
        mark = {"same": "✅", "overlap": "🟡", "disjoint": "❌"}[entry["verdict"]]
        lines.append(
            f"| {entry['map']} | {entry['seed']} | {entry['p1']} | {entry['p2']} | "
            + " | ".join(cells)
            + f" | {mark} |"
        )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--variant", action="append", default=[], help="NAME=BIN（複数指定可）")
    parser.add_argument("--env", action="append", default=[], help="NAME:KEY=VALUE（変種ごとの環境変数、複数指定可）")
    parser.add_argument("--map", required=True, help="カンマ区切りのマップ（例: map_1,map_6）")
    parser.add_argument("--seeds", default=None, help="カンマ区切りのseed（例: 1,2）")
    parser.add_argument("--seed", type=int, default=None, help="単一seed（--seedsと排他）")
    parser.add_argument("--p1", default="V4", help="評価対象のAIバージョン")
    parser.add_argument("--p2", default="V200", help="相手のAIバージョン")
    parser.add_argument("--player-order", choices=["both", "as-given", "swapped"], default="both")
    parser.add_argument("--games", type=int, default=1, help="seedごとの局数")
    parser.add_argument("--repeats", type=int, default=1, help="同じ条件を繰り返す回数（実行ごとの揺れの分布を見る）")
    parser.add_argument("--max-turns", type=int, default=30)
    parser.add_argument("--grid-type", choices=["square", "hex"], default="hex")
    parser.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 2), help="同時に動かす対局数（=サーバー数）")
    parser.add_argument("--out-dir", required=True, help="出力先ディレクトリ")
    args = parser.parse_args()

    try:
        variants = parse_variants(args.variant, args.env)
        if args.seeds and args.seed is not None:
            raise ValueError("--seeds and --seed are mutually exclusive")
        seed_list = parse_seed_list(args.seeds) if args.seeds else (args.seed,)
        if args.jobs < 1 or args.repeats < 1 or args.games < 1:
            raise ValueError("--jobs, --repeats and --games must be >= 1")
    except ValueError as error:
        parser.error(str(error))

    maps = tuple(m.strip() for m in args.map.split(",") if m.strip())
    seeds = tuple(seed for seed in seed_list for _ in range(args.games))
    specs = build_match_specs(
        maps, args.p1, args.p2, seeds, grid_type=args.grid_type, player_order=args.player_order
    )
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    jobs = build_jobs(variants, specs, args.repeats)
    registry = matchup_runner.ServerRegistry()
    variant_names = list(variants)
    rows: list[dict] = []
    slim_by_variant: dict[str, list[dict]] = defaultdict(list)
    done = [0]
    metadata = {
        "maps": maps,
        "seeds": seed_list,
        "subject": args.p1,
        "baseline": args.p2,
        "player_order": args.player_order,
        "games_per_seed": args.games,
        "repeats": args.repeats,
        "max_turns": args.max_turns,
        "grid_type": args.grid_type,
        "variants": variants,
    }

    def make_client(job: dict, number: int) -> matchup_runner.McpServerClient:
        variant = variants[job["variant"]]
        return registry.create(
            [variant["bin"]],
            env_overrides=variant["env"],
            stderr_path=out_dir / "logs" / "servers" / f"{job['variant']}-{number}.stderr.log",
        )

    def game_name(job: dict) -> str:
        return f"{condition_dir(job, args.games)}/seed{job['seed']}"

    def run_one(job: dict, client: matchup_runner.McpServerClient) -> dict:
        game_log = matchup_runner.GameLog(out_dir / "logs" / job["variant"] / f"{game_name(job)}.log")
        try:
            result = run_single_game(
                job["map"],
                job["p1"],
                job["p2"],
                args.max_turns,
                seed=job["seed"],
                grid_type=job["grid_type"],
                ui_callback=game_log,
                tool_caller=client.call_tool,
            )
        finally:
            game_log.close()
        result.update({key: job[key] for key in ("map", "p1", "p2", "seed", "grid_type")})
        result["variant"] = job["variant"]
        result["repeat"] = job["repeat"]
        # 生データは完了時にファイルへ書き、メモリには集計に要る軽量版だけを残す。
        write_json_atomic(
            out_dir / "games" / job["variant"] / f"{game_name(job)}.json",
            {
                "metadata": dict(metadata, variant=job["variant"], repeat=job["repeat"], criteria=None),
                "results": [result],
            },
        )
        slim = matchup_runner.slim_result(result)
        # ターンごとの数値推移は変種別レポートが使う。小さいので軽量版にも残す。
        slim["metrics"] = result.get("metrics", [])
        slim["action_hash"] = action_hash(result)
        return slim

    def on_done(index: int, job: dict, slim: dict, elapsed: float) -> None:
        done[0] += 1
        rows.append(slim)
        slim_by_variant[job["variant"]].append(slim)
        label = f"{job['variant']} {game_name(job)} [{slim['action_hash']}]"
        matchup_runner.print_flush(matchup_runner.format_progress(done[0], len(jobs), label, slim, elapsed))

    matchup_runner.print_flush(
        f"sweep: {len(variants)} variants x {len(specs)} games x {args.repeats} repeats = {len(jobs)} games, jobs={args.jobs}"
    )
    try:
        matchup_runner.run_jobs(
            jobs,
            args.jobs,
            server_key=lambda job: job["variant"],
            make_client=make_client,
            run_one=run_one,
            on_done=on_done,
        )
    finally:
        # 正常終了・例外・Ctrl+C のいずれでも、このプロセスが起動したサーバーだけを止める。
        registry.close_all()
        # 途中終了でも、完了済みの対局から集計を残す。
        summary = summarize(rows, variant_names)
        write_json_atomic(out_dir / "summary.json", {"metadata": metadata, "summary": summary})
        write_summary_markdown(out_dir / "summary.md", summary, variant_names, args.repeats)
        for name, results in slim_by_variant.items():
            report = generate_report(results, subject=args.p1, baseline=args.p2, max_turns=args.max_turns)
            (out_dir / f"{name}.md").write_text(report, encoding="utf-8")

    verdicts = Counter(entry["verdict"] for entry in summary)
    matchup_runner.print_flush(
        f"done: {len(rows)}/{len(jobs)} games, verdicts same={verdicts['same']} "
        f"overlap={verdicts['overlap']} disjoint={verdicts['disjoint']} -> {out_dir / 'summary.md'}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
