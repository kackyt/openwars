import argparse
import subprocess
import json
import os
import sys
import time
import re
from collections import defaultdict

try:
    from rich.live import Live
    from rich.table import Table
    from rich.panel import Panel
    from rich.layout import Layout
    HAS_RICH = True
except ImportError:
    HAS_RICH = False

p = None  # MCP Server process

def init_mcp_server():
    global p
    env = os.environ.copy()
    env['RUST_LOG'] = 'info'
    os.system('taskkill /F /IM mcp-server.exe >nul 2>&1')
    p = subprocess.Popen(
        ['target/release/mcp-server.exe'],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, # エラー出力を無視して画面崩れを防ぐ
        text=True,
        encoding='utf-8',
        env=env
    )
    
    # MCP 初期化シーケンス
    init_params = {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {
            "name": "openwars-eval",
            "version": "0.1.0"
        }
    }
    send_request("initialize", init_params, req_id=0)
    receive_response()

def send_request(method, params=None, req_id=1):
    req = {
        "jsonrpc": "2.0",
        "id": req_id,
        "method": method
    }
    if params:
        req["params"] = params
    line = json.dumps(req)
    p.stdin.write(line + '\n')
    p.stdin.flush()

def receive_response():
    line = p.stdout.readline()
    if not line:
        return None
    return json.loads(line)

def call_tool(name, arguments=None, req_id=1):
    params = {
        "name": name,
        "arguments": arguments or {}
    }
    send_request("tools/call", params, req_id)
    res = receive_response()
    if not res:
        return None
    if "error" in res:
        raise Exception(f"Tool {name} returned error: {res['error']}")
    
    result_data = res.get("result", {})
    if result_data.get("isError"):
        return {"error": result_data.get("content")}
        
    content = result_data.get('content', [{'text': '{}'}])[0]['text']
    try:
        return json.loads(content)
    except json.JSONDecodeError:
        return content

def run_single_game(map_name, p1_ver, p2_ver, max_turns, ui_callback=None):
    if ui_callback: ui_callback({"type": "log", "msg": f"Match started: P1({p1_ver}) vs P2({p2_ver}) on {map_name}"})
    
    call_tool("load_map", {"map_name": map_name})
    call_tool("set_player_ai_version", {"player_id": 1, "version": p1_ver})
    call_tool("set_player_ai_version", {"player_id": 2, "version": p2_ver})

    thinking_times = {1: [], 2: []}
    action_counts = {1: defaultdict(int), 2: defaultdict(int)}
    metrics = []

    turn = 1
    while turn <= max_turns:
        
        state = call_tool("get_board_state")
        if isinstance(state, dict) and state.get("error"):
            if ui_callback: ui_callback({"type": "log", "msg": f"Error: {state['error']}"})
            break

        game_over = state.get("game_over")
        if game_over:
            status = game_over.get("status")
            if status == "winner":
                winner_id = game_over.get("winner_id")
                if ui_callback: ui_callback({"type": "log", "msg": f"Game Finished! Winner: P{winner_id}"})
                return {
                    "result": f"P{winner_id}_Win",
                    "turns": turn,
                    "thinking_times": thinking_times,
                    "action_counts": action_counts,
                    "metrics": metrics,
                    "final_state": state
                }
            elif status == "draw":
                if ui_callback: ui_callback({"type": "log", "msg": f"Game Finished! Draw"})
                return {
                    "result": "Draw",
                    "turns": turn,
                    "thinking_times": thinking_times,
                    "action_counts": action_counts,
                    "metrics": metrics,
                    "final_state": state
                }

        # ターンごとのメトリクス収集
        p_info = state.get("players", [])
        m = {"turn": turn, "p1_props": 0, "p2_props": 0, "p1_units": 0, "p2_units": 0, "p1_funds": 0, "p2_funds": 0, "p1_score": 0, "p2_score": 0}
        # 主観的評価値の取得（AIバージョンによって異なる、AIが現在考えている有利不利）
        s1 = call_tool("evaluate_board", {"player_id": 1})
        s2 = call_tool("evaluate_board", {"player_id": 2})
        m["p1_score"] = s1.get("score", 0) if isinstance(s1, dict) else 0
        m["p2_score"] = s2.get("score", 0) if isinstance(s2, dict) else 0

        for player in p_info:
            pid = player.get("player_id")
            if pid == 1:
                p1_props = player.get("property_count", 0)
                p1_units = player.get("unit_cost", 0)
                m["p1_props"] = p1_props
                m["p1_units"] = p1_units
                m["p1_funds"] = player.get("funds", 0)
                m["p1_abs_score"] = p1_props * 20000 + p1_units
            elif pid == 2:
                p2_props = player.get("property_count", 0)
                p2_units = player.get("unit_cost", 0)
                m["p2_props"] = p2_props
                m["p2_units"] = p2_units
                m["p2_funds"] = player.get("funds", 0)
                m["p2_abs_score"] = p2_props * 20000 + p2_units
        metrics.append(m)
        if ui_callback: ui_callback({"type": "status_update", "metrics": m})

        active_idx = state.get("active_player_index", 0)
        if ui_callback and active_idx == 0:
            ui_callback({"type": "turn_start", "turn": turn, "max_turns": max_turns})

        current_player = state["players"][active_idx]["player_id"]
        
        t0 = time.time()
        ai_result = call_tool("simulate_ai_turn")
        t1 = time.time()
        
        thinking_ms = (t1 - t0) * 1000
        thinking_times[current_player].append(thinking_ms)
        
        if isinstance(ai_result, dict) and ai_result.get("error"):
            if ui_callback: ui_callback({"type": "log", "msg": f"AI Error: {ai_result['error']}"})
            break
            
        actions = ai_result.get("actions_taken", [])
        acts_dict = defaultdict(int)
        for action in actions:
            action_str = str(action)
            match = re.search(r"ProduceUnitCommand\s*\{\s*player_id:\s*PlayerId\((\d+)\),\s*.*unit_type:\s*(\w+)", action_str)
            if match:
                pid = int(match.group(1))
                utype = match.group(2)
                action_counts[pid][utype] += 1
                acts_dict[f"Produce {utype}"] += 1
            elif "MoveUnitCommand" in action_str:
                acts_dict["Move"] += 1
            elif "AttackUnitCommand" in action_str:
                acts_dict["Attack"] += 1
            elif "CapturePropertyCommand" in action_str:
                acts_dict["Capture"] += 1
            elif "LoadUnitCommand" in action_str:
                acts_dict["Load"] += 1
            elif "DropUnitCommand" in action_str:
                acts_dict["Drop"] += 1

        if ui_callback and acts_dict:
            act_str = ", ".join([f"{k}({v})" for k, v in acts_dict.items()])
            ui_callback({"type": "log", "msg": f"P{current_player} T{turn}: {act_str}"})

        if active_idx == 1:
            turn += 1

    if ui_callback: ui_callback({"type": "log", "msg": "Max turns reached. Calculating state value decision..."})
    
    p1_final = metrics[-1]["p1_abs_score"] if metrics else 0
    p2_final = metrics[-1]["p2_abs_score"] if metrics else 0
    
    if p1_final > p2_final:
        result_str = "P1_Win_MaxTurns"
        winner_id = 1
    elif p2_final > p1_final:
        result_str = "P2_Win_MaxTurns"
        winner_id = 2
    else:
        result_str = "Draw_MaxTurns"
        winner_id = None

    if ui_callback:
        if winner_id:
            ui_callback({"type": "log", "msg": f"Game Finished! Winner: P{winner_id} (Absolute Score: {p1_final} vs {p2_final})"})
        else:
            ui_callback({"type": "log", "msg": f"Game Finished! Draw (Absolute Score: {p1_final} vs {p2_final})"})

    return {
        "result": result_str,
        "turns": max_turns,
        "thinking_times": thinking_times,
        "action_counts": action_counts,
        "metrics": metrics,
        "final_state": state
    }

def generate_report(results):
    v2_wins = 0
    v1_wins = 0
    draws = 0
    total_games = len(results)
    
    thinking_times_v2 = []
    thinking_times_v1 = []
    v2_win_turns = []
    v1_win_turns = []
    map_summaries = defaultdict(list)
    
    for game in results:
        p1, p2 = game["p1"], game["p2"]
        res = game["result"]
        if "P1_Win" in res:
            if p1 == "V2": v2_wins += 1; v2_win_turns.append(game["turns"])
            else: v1_wins += 1; v1_win_turns.append(game["turns"])
        elif "P2_Win" in res:
            if p2 == "V2": v2_wins += 1; v2_win_turns.append(game["turns"])
            else: v1_wins += 1; v1_win_turns.append(game["turns"])
        else:
            draws += 1
            
        t1 = game.get("thinking_times", {}).get(1, [])
        t2 = game.get("thinking_times", {}).get(2, [])
        if p1 == "V2": thinking_times_v2.extend(t1)
        else: thinking_times_v1.extend(t1)
        if p2 == "V2": thinking_times_v2.extend(t2)
        else: thinking_times_v1.extend(t2)
        
        map_summaries[game["map"]].append({
            "matchup": f"P1({p1}) vs P2({p2})",
            "result": res,
            "turns": game["turns"],
            "p1_actions": game.get("action_counts", {}).get(1, {}),
            "p2_actions": game.get("action_counts", {}).get(2, {}),
            "metrics": game.get("metrics", [])
        })
        
    v2_win_rate = (v2_wins / total_games) * 100 if total_games > 0 else 0
    avg_turns_v2 = sum(v2_win_turns) / len(v2_win_turns) if v2_win_turns else 0
    avg_turns_v1 = sum(v1_win_turns) / len(v1_win_turns) if v1_win_turns else 0
    avg_time_v2 = sum(thinking_times_v2) / len(thinking_times_v2) if thinking_times_v2 else 0
    avg_time_v1 = sum(thinking_times_v1) / len(thinking_times_v1) if thinking_times_v1 else 0
    
    report = ["# 🏆 AI Matchup Evaluator Report"]
    report.append("## 📊 総合結果サマリー")
    report.append(f"- **総対戦数**: {total_games} ゲーム")
    report.append(f"- **V2 (新AI) の総合勝率**: **{v2_win_rate:.1f}%** ({v2_wins}勝 {v1_wins}敗 {draws}分)")
    report.append(f"- **平均勝利ターン数**: ")
    report.append(f"  - **V2 (新AI) 勝利時**: {avg_turns_v2:.1f} ターン")
    report.append(f"  - **V1 (旧AI) 勝利時**: {avg_turns_v1:.1f} ターン")
    report.append(f"- **平均思考時間 (1ターンあたり)**: ")
    report.append(f"  - **V2 (新AI)**: **{avg_time_v2:.1f} ms**")
    report.append(f"  - **V1 (旧AI)**: **{avg_time_v1:.1f} ms**\n")
    
    for map_name, games in map_summaries.items():
        report.append(f"### 📍 {map_name}")
        report.append("| 対戦カード | 結果 | ターン数 | P1 生産内訳 | P2 生産内訳 |")
        report.append("| :--- | :--- | :--- | :--- | :--- |")
        for g in games:
            p1_act = "<br>".join([f"{k}: {v}" for k, v in g['p1_actions'].items()]) if g['p1_actions'] else "None"
            p2_act = "<br>".join([f"{k}: {v}" for k, v in g['p2_actions'].items()]) if g['p2_actions'] else "None"
            report.append(f"| {g['matchup']} | **{g['result']}** | {g['turns']} | {p1_act} | {p2_act} |")
        report.append("\n")
        
    return "\n".join(report)

def main():
    parser = argparse.ArgumentParser(description="AI Matchup Evaluator for OpenWars")
    parser.add_argument("--mode", choices=["tui", "batch"], default="tui", help="Execution mode (tui or batch)")
    parser.add_argument("--map", default="map_3", help="Map to test")
    parser.add_argument("--p1", default="V2", help="Player 1 AI Version")
    parser.add_argument("--p2", default="V1", help="Player 2 AI Version")
    parser.add_argument("--games", type=int, default=1, help="Number of games per matchup")
    parser.add_argument("--max-turns", type=int, default=30, help="Maximum turns per game")
    parser.add_argument("--output", default="matchup_report.md", help="Output file for the final report")
    args = parser.parse_args()

    init_mcp_server()
    all_results = []
    logs = []
    
    def ui_callback_tui(event, layout, live):
        if event["type"] == "log":
            logs.append(event["msg"])
            if len(logs) > 10: logs.pop(0)
            layout["log"].update(Panel("\n".join(logs), title="Log"))
        elif event["type"] == "turn_start":
            pass
        elif event["type"] == "status_update":
            m = event["metrics"]
            t = Table(show_header=True, header_style="bold magenta")
            t.add_column("Player")
            t.add_column("Funds")
            t.add_column("Properties")
            t.add_column("Unit Value (NPV)")
            t.add_column("AI Eval Score")
            t.add_column("Absolute Score")
            t.add_row(f"P1 ({args.p1})", str(m["p1_funds"]), str(m["p1_props"]), str(m["p1_units"]), str(m.get("p1_score", 0)), str(m.get("p1_abs_score", 0)))
            t.add_row(f"P2 ({args.p2})", str(m["p2_funds"]), str(m["p2_props"]), str(m["p2_units"]), str(m.get("p2_score", 0)), str(m.get("p2_abs_score", 0)))
            layout["status"].update(Panel(t, title=f"Turn {m['turn']}"))
        
        live.refresh()

    def ui_callback_batch(event):
        if event["type"] == "log":
            print(f"[LOG] {event['msg']}")
        elif event["type"] == "status_update":
            m = event["metrics"]
            print(json.dumps({"type": "metrics", "data": m}))
            
    try:
        if args.mode == "tui" and HAS_RICH:
            layout = Layout()
            layout.split_column(
                Layout(name="header", size=3),
                Layout(name="status", size=8),
                Layout(name="log")
            )
            layout["header"].update(Panel(f"[bold cyan]OpenWars AI Matchup: {args.p1} vs {args.p2} on {args.map}[/bold cyan]"))
            layout["status"].update(Panel("Waiting for game to start..."))
            layout["log"].update(Panel("Logs will appear here..."))
            
            with Live(layout, refresh_per_second=4) as live:
                for i in range(args.games):
                    res = run_single_game(args.map, args.p1, args.p2, args.max_turns, lambda e: ui_callback_tui(e, layout, live))
                    res["map"] = args.map
                    res["p1"] = args.p1
                    res["p2"] = args.p2
                    all_results.append(res)
                    
                    res2 = run_single_game(args.map, args.p2, args.p1, args.max_turns, lambda e: ui_callback_tui(e, layout, live))
                    res2["map"] = args.map
                    res2["p1"] = args.p2
                    res2["p2"] = args.p1
                    all_results.append(res2)
        else:
            print(json.dumps({"type": "info", "msg": f"Starting batch run: {args.p1} vs {args.p2} on {args.map} ({args.games} games)"}))
            for i in range(args.games):
                res = run_single_game(args.map, args.p1, args.p2, args.max_turns, ui_callback_batch)
                res["map"] = args.map
                res["p1"] = args.p1
                res["p2"] = args.p2
                all_results.append(res)
                print(json.dumps({"type": "result", "data": res}))
                
                res2 = run_single_game(args.map, args.p2, args.p1, args.max_turns, ui_callback_batch)
                res2["map"] = args.map
                res2["p1"] = args.p2
                res2["p2"] = args.p1
                all_results.append(res2)
                print(json.dumps({"type": "result", "data": res2}))
                
        report = generate_report(all_results)
        with open(args.output, "w", encoding="utf-8") as f:
            f.write(report)
            
        if args.mode == "batch":
            print(json.dumps({"type": "info", "msg": f"Report generated at {args.output}"}))

    finally:
        if p:
            p.stdin.close()
            p.wait()

if __name__ == "__main__":
    main()
