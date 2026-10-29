"""対局ログの途中の盤面から、両軍のAIで数ラウンドだけ対局を進める分岐シミュレーションを並列に回す。

60ターンの対局スイープを回す前に、問題の起きている局面（例: 10ラウンド目の西側の停滞）だけで
修正の効果を確かめるための道具。中身は `engine/tests/branch_sim.rs`（ignoredテスト）で、
本スクリプトはそれをビルドし、ログディレクトリ内の `seed*.json` ごとに1プロセスずつ起動する。

使用例（スイープのログ12局を、10ラウンド目から8ラウンド分、変更前後で比べる）:

    python scripts/run_branch_sim.py \\
        --logs .tmp/sweep-YYYYMMDD/games/current/map_20_V200-V4_r0 \\
        --from 10 --turns 8 --jobs 4 --out-dir .tmp/branch/after \\
        --env OPENWARS_CONCENTRATE=1

    python scripts/compare_branch_sim.py --rounds 13,17 \\
        actual=.tmp/sweep-YYYYMMDD/games/current/map_20_V200-V4_r0 \\
        before=.tmp/branch/before after=.tmp/branch/after

変更前（before）は、変更前のコミットの作業ツリーで同じコマンドを回す。テストバイナリは
`--bin` で固定したものも渡せる（実行中に別のビルドで上書きされるのを避けたいとき）。

出力（`--out-dir` 配下）:

- `seed<N>.json` … `{"results": [{"strategic_history": [...], "from": F, "turns": T}]}`。
  `strategic_history` は対局ログと同じ形式（各ラウンドの終わりの盤面）なので、
  `compare_branch_sim.py` で対局ログと同じように読める。
- `seed<N>.log` … テストバイナリの標準出力・標準エラー。
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


def build_test_binary() -> Path:
    """branch_sim テストを release でビルドし、実行ファイルのパスを返す。"""
    command = [
        "cargo",
        "test",
        "--release",
        "-p",
        "engine",
        "--test",
        "branch_sim",
        "--no-run",
        "--message-format=json",
    ]
    completed = subprocess.run(
        command, cwd=REPO_ROOT, stdout=subprocess.PIPE, check=True, text=True
    )
    # cargo の JSON メッセージから、テストのプロファイルでビルドされた branch_sim の実行ファイルを探す
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == "branch_sim"
            and message.get("profile", {}).get("test")
            and message.get("executable")
        ):
            return Path(message["executable"])
    raise RuntimeError("branch_sim のテストバイナリが見つからない")


def parse_env(pairs: list[str]) -> dict[str, str]:
    """`KEY=VALUE` の並びを辞書にする。"""
    env = {}
    for pair in pairs:
        key, sep, value = pair.partition("=")
        if not sep or not key:
            raise SystemExit(f"--env は KEY=VALUE の形で指定する: {pair}")
        env[key] = value
    return env


def run_one(
    binary: Path, log: Path, out_dir: Path, base_env: dict[str, str]
) -> tuple[Path, int]:
    """1局分の分岐シミュレーションを回し、(ログ, 終了コード) を返す。"""
    out_json = out_dir / log.name
    env = dict(base_env)
    env["BRANCH_LOG"] = str(log.resolve())
    env["BRANCH_OUT"] = str(out_json.resolve())
    with open(out_dir / f"{log.stem}.log", "w", encoding="utf-8") as stream:
        completed = subprocess.run(
            [str(binary), "--ignored", "--nocapture"],
            cwd=REPO_ROOT / "engine",
            env=env,
            stdout=stream,
            stderr=subprocess.STDOUT,
            check=False,
        )
    return log, completed.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--logs",
        required=True,
        help="seed*.json（対局ログ）を含むディレクトリ、または1つのjsonファイル",
    )
    parser.add_argument("--out-dir", required=True, help="結果の出力先ディレクトリ")
    parser.add_argument(
        "--from", dest="from_round", type=int, default=10, help="分岐するラウンド"
    )
    parser.add_argument("--turns", type=int, default=8, help="進めるラウンド数")
    parser.add_argument(
        "--warmup",
        type=int,
        default=3,
        help="記憶を温めるために判断を再現するラウンド数",
    )
    parser.add_argument("--map", default="map_20", help="マップ名")
    parser.add_argument(
        "--player", type=int, default=2, help="V4 で動かす側（相手は V200）"
    )
    parser.add_argument("--seed", type=int, default=1, help="乱数シード")
    parser.add_argument(
        "--env",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="AIに渡す環境変数（繰り返し指定可）",
    )
    parser.add_argument("--jobs", type=int, default=4, help="並列数")
    parser.add_argument(
        "--bin", help="ビルド済みのテストバイナリ（省略時はここでビルドする）"
    )
    args = parser.parse_args()

    logs_path = Path(args.logs)
    logs = [logs_path] if logs_path.is_file() else sorted(logs_path.glob("seed*.json"))
    if not logs:
        raise SystemExit(f"seed*.json が見つからない: {logs_path}")
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    # 実行時の作業ディレクトリは engine/ なので、相対パスはここで絶対パスにする
    binary = Path(args.bin).resolve() if args.bin else build_test_binary()
    base_env = dict(os.environ)
    base_env.update(
        {
            "BRANCH_FROM": str(args.from_round),
            "BRANCH_TURNS": str(args.turns),
            "BRANCH_WARMUP": str(args.warmup),
            "BRANCH_MAP": args.map,
            "BRANCH_PLAYER": str(args.player),
            "BRANCH_SEED": str(args.seed),
        }
    )
    base_env.update(parse_env(args.env))

    failures = 0
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        futures = [pool.submit(run_one, binary, log, out_dir, base_env) for log in logs]
        for future in futures:
            log, code = future.result()
            status = "ok" if code == 0 else f"失敗 (exit {code})"
            print(f"{log.name}: {status}", flush=True)
            failures += code != 0
    print(f"完了: {len(logs) - failures}/{len(logs)} 局 → {out_dir}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
