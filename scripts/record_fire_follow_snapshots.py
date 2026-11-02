#!/usr/bin/env python3
"""対象AIの自手番ごとに、手番前後の盤面・イベント・首都進軍路を保存する記録器。

`eval_matchup.run_single_game` は変更せず、`tool_caller` を包んで
`simulate_ai_turn` の直前（ゲームループが取得した盤面）と直後の盤面を保存する。
出力は `scripts/review_regional_execution.py` がそのまま読める `snapshots` 形式である。

    python scripts/record_fire_follow_snapshots.py --map map_20 --p1 V4 --p2 V200 \\
        --seed 1 --max-turns 60 --mcp-server-bin .tmp/order/current/mcp-server \\
        --out .tmp/order/map20-p1.json
"""

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import eval_matchup  # noqa: E402
import matchup_runner  # noqa: E402


def slim_board(state):
    """集計に必要な盤面の項目だけを残す（記録の肥大化を避ける）。"""
    return {
        "turn": state.get("turn"),
        "units": state.get("units", []),
        "properties": state.get("properties", []),
    }


class SnapshotRecorder:
    """ツール呼び出しを中継し、対象プレイヤーの自手番だけを記録する。"""

    def __init__(self, client, subject_player):
        self.client = client
        self.subject_player = subject_player
        self.last_state = None
        self.snapshots = []

    def __call__(self, name, arguments=None):
        result = self.client.call_tool(name, arguments)
        if name == "get_board_state" and isinstance(result, dict):
            self.last_state = result
        elif name == "simulate_ai_turn" and isinstance(result, dict) and self.last_state:
            players = self.last_state.get("players", [])
            index = self.last_state.get("active_player_index", 0)
            player = result.get("player_id", players[index]["player_id"] if players else None)
            if player == self.subject_player:
                after = self.client.call_tool("get_board_state")
                self.snapshots.append({
                    "player_id": player,
                    "before": slim_board(self.last_state),
                    "after": slim_board(after if isinstance(after, dict) else {}),
                    "events": result.get("invasion_events", []),
                    "routes": result.get("capital_routes") or {},
                })
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--map", required=True)
    parser.add_argument("--p1", default="V4")
    parser.add_argument("--p2", default="V200")
    parser.add_argument("--subject", default="V4", help="記録する側のAI版")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--max-turns", type=int, default=60)
    parser.add_argument("--grid-type", default="hex")
    parser.add_argument("--mcp-server-bin", required=True)
    parser.add_argument("--env", action="append", default=[], help="KEY=VALUE（サーバーへ渡す）")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    subject = eval_matchup.normalize_ai_version(args.subject)
    p1 = eval_matchup.normalize_ai_version(args.p1)
    p2 = eval_matchup.normalize_ai_version(args.p2)
    if subject not in (p1, p2) or p1 == p2:
        parser.error("--subject は p1/p2 の一方だけに一致させる")
    subject_player = 1 if p1 == subject else 2
    env = dict(item.split("=", 1) for item in args.env)
    client = matchup_runner.McpServerClient(
        eval_matchup.server_command(args.mcp_server_bin), env_overrides=env)
    recorder = SnapshotRecorder(client, subject_player)
    try:
        game = eval_matchup.run_single_game(
            args.map, p1, p2, args.max_turns, seed=args.seed,
            grid_type=args.grid_type, tool_caller=recorder)
    finally:
        client.close()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps({
        "map": args.map, "p1": p1, "p2": p2, "seed": args.seed,
        "subject_player": subject_player,
        "result": game.get("result"), "turns": game.get("turns"), "error": game.get("error"),
        "snapshots": recorder.snapshots,
    }, ensure_ascii=False))
    print(json.dumps({"out": str(args.out), "result": game.get("result"),
                      "turns": game.get("turns"), "snapshots": len(recorder.snapshots),
                      "error": game.get("error")}, ensure_ascii=False))


if __name__ == "__main__":
    main()
