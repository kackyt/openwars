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
    # スクリプト位置基準でリポジトリルートの実行ファイルを絶対パス解決する
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe_path = os.path.join(repo_root, 'target', 'release', 'mcp-server.exe')
    p = subprocess.Popen(
        [exe_path],
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
        # 主観メトリクス（AIバージョン依存の評価内訳。形勢認識の分析用）
        m["p1_subj"] = s1.get("subjective_metrics", {}) if isinstance(s1, dict) else {}
        m["p2_subj"] = s2.get("subjective_metrics", {}) if isinstance(s2, dict) else {}
        # 客観メトリクス（バージョン非依存。合否判定・ジリ貧分析用）
        m["p1_obj"] = s1.get("objective_metrics", {}) if isinstance(s1, dict) else {}
        m["p2_obj"] = s2.get("objective_metrics", {}) if isinstance(s2, dict) else {}

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

def moving_average(values, window=5):
    """5ターン移動平均（先頭は利用可能な範囲で平均）"""
    out = []
    for i in range(len(values)):
        w = values[max(0, i - window + 1):i + 1]
        out.append(sum(w) / len(w))
    return out


def check_no_decline(series, start_turn=15):
    """5ターン移動平均が start_turn 以降に減少トレンドへ転じていないか。
    判定: 最終時点の移動平均が start_turn 時点の移動平均以上であること。
    start_turn 未満で決着したゲームはジリ貧とは見なさない。"""
    if len(series) < start_turn:
        return True
    ma = moving_average(series)
    return ma[-1] >= ma[start_turn - 1]


def judge_objective_criteria(results):
    """確定済みの客観メトリクス基準で合否判定する。
    基準1: 判定時点(30T or 決着時点)の ZOC支配面積 V2平均 > V1平均（先攻・後攻それぞれ）
    基準2: 同・ターン収入
    基準3: V2のユニット資産価値・収入の5T移動平均が15T以降に減少トレンドへ転じない
           （ZOCは終盤のユニット密集で重複減少し誤検知するため、ストック指標で判定する。
            ZOCの優位性自体は基準1でカバーされる）
    戻り値: (per_map判定dict, 全体PASS/FAIL, 詳細行リスト)"""
    # (map, order) -> 集計
    buckets = defaultdict(lambda: {"v2_zoc": [], "v1_zoc": [], "v2_inc": [], "v1_inc": [], "trend_ok": []})

    for g in results:
        p1, p2 = g["p1"], g["p2"]
        if "V2" not in (p1, p2) or p1 == p2:
            continue
        v2_side = "p1" if p1 == "V2" else "p2"
        v1_side = "p2" if v2_side == "p1" else "p1"
        order = "先攻" if v2_side == "p1" else "後攻"
        raw_metrics = g.get("metrics", [])
        if not raw_metrics:
            continue
        # 1ターンに両手番分の2エントリが記録されるため、ターンごとに最後のエントリへ集約する
        by_turn = {}
        for m in raw_metrics:
            by_turn[m["turn"]] = m
        metrics = [by_turn[t] for t in sorted(by_turn)]
        last = metrics[-1]
        b = buckets[(g["map"], order)]
        b["v2_zoc"].append(last.get(f"{v2_side}_obj", {}).get("zoc_area", 0))
        b["v1_zoc"].append(last.get(f"{v1_side}_obj", {}).get("zoc_area", 0))
        b["v2_inc"].append(last.get(f"{v2_side}_obj", {}).get("income_per_turn", 0))
        b["v1_inc"].append(last.get(f"{v1_side}_obj", {}).get("income_per_turn", 0))
        # ユニット資産価値 (unit_cost合計) はゲームステート由来の p1_units/p2_units を使う
        asset_series = [m.get(f"{v2_side}_units", 0) for m in metrics]
        asset_ok = check_no_decline(asset_series)
        inc_series = [m.get(f"{v2_side}_obj", {}).get("income_per_turn", 0) for m in metrics]
        inc_ok = check_no_decline(inc_series)
        b["trend_ok"].append(asset_ok and inc_ok)

    def avg(xs):
        return sum(xs) / len(xs) if xs else 0

    detail_rows = []
    map_pass = {}
    for (map_name, order), b in sorted(buckets.items()):
        c1 = avg(b["v2_zoc"]) > avg(b["v1_zoc"])
        c2 = avg(b["v2_inc"]) > avg(b["v1_inc"])
        c3 = all(b["trend_ok"]) if b["trend_ok"] else False
        detail_rows.append({
            "map": map_name, "order": order,
            "v2_zoc": avg(b["v2_zoc"]), "v1_zoc": avg(b["v1_zoc"]), "c1": c1,
            "v2_inc": avg(b["v2_inc"]), "v1_inc": avg(b["v1_inc"]), "c2": c2,
            "c3": c3,
        })
        ok = c1 and c2 and c3
        map_pass[map_name] = map_pass.get(map_name, True) and ok

    overall = all(map_pass.values()) if map_pass else False
    return map_pass, overall, detail_rows


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

    # 客観メトリクス基準の合否判定 (Issue #48 確定基準)
    map_pass, overall, detail_rows = judge_objective_criteria(results)
    report.append("## ✅ 合否判定（客観メトリクス基準）")
    report.append("判定時点 = 各戦の30ターン時点（それ以前に決着した場合は決着時点）。")
    report.append("")
    report.append("| マップ | 手番 | 基準1: ZOC支配面積 (V2 vs V1) | 基準2: ターン収入 (V2 vs V1) | 基準3: ジリ貧解消 | 判定 |")
    report.append("| :--- | :--- | :--- | :--- | :--- | :--- |")
    for r in detail_rows:
        c1s = f"{'✅' if r['c1'] else '❌'} {r['v2_zoc']:.1f} vs {r['v1_zoc']:.1f}"
        c2s = f"{'✅' if r['c2'] else '❌'} {r['v2_inc']:.0f} vs {r['v1_inc']:.0f}"
        c3s = '✅' if r['c3'] else '❌'
        ok = '**PASS**' if (r['c1'] and r['c2'] and r['c3']) else '**FAIL**'
        report.append(f"| {r['map']} | {r['order']} | {c1s} | {c2s} | {c3s} | {ok} |")
    report.append("")
    report.append(f"**全体判定: {'✅ PASS' if overall else '❌ FAIL'}**" )
    report.append("")

    report.append("## 📊 総合結果サマリー")
    report.append(f"- **総対戦数**: {total_games} ゲーム")
    report.append(f"- **V2 (新AI) の総合勝率（参考・ガードレール40%）**: **{v2_win_rate:.1f}%** ({v2_wins}勝 {v1_wins}敗 {draws}分)")
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
        
        # 客観メトリクス（支配面積・収入・NPV）の推移表
        report.append("#### 📈 客観メトリクス (Objective Metrics) の推移")
        for g in games:
            report.append(f"*{g['matchup']} (Result: {g['result']})*")
            report.append("| ターン | P1 ZOC面積 | P1 収入 | P1 拠点 | P1 NPV | P1 主観スコア | P2 ZOC面積 | P2 収入 | P2 拠点 | P2 NPV | P2 主観スコア |")
            report.append("| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |")
            for i, m in enumerate(g['metrics']):
                if i % 5 == 0 or i == len(g['metrics']) - 1:  # 5ターンごと、または最終ターン
                    o1 = m.get("p1_obj", {})
                    o2 = m.get("p2_obj", {})
                    report.append(
                        f"| {m['turn']} | {o1.get('zoc_area', 0)} | {o1.get('income_per_turn', 0)} | {o1.get('owned_properties', 0)} | {o1.get('npv', 0)} | {m.get('p1_score', 0)} "
                        f"| {o2.get('zoc_area', 0)} | {o2.get('income_per_turn', 0)} | {o2.get('owned_properties', 0)} | {o2.get('npv', 0)} | {m.get('p2_score', 0)} |"
                    )
            # 戦闘効率 (ROI): 最終時点の累計与/被ダメージ価値
            if g['metrics']:
                last = g['metrics'][-1]
                roi_parts = []
                for side in ("p1", "p2"):
                    o = last.get(f"{side}_obj", {})
                    dealt = o.get("combat_value_dealt", 0)
                    received = o.get("combat_value_received", 0)
                    eff = f"{dealt / received:.2f}" if received > 0 else "-"
                    roi_parts.append(f"{side.upper()}: 与{dealt} / 被{received} (効率 {eff})")
                report.append("")
                report.append(f"戦闘効率(ROI): {' , '.join(roi_parts)}")
            report.append("\n")

    return "\n".join(report)

def main():
    parser = argparse.ArgumentParser(description="AI Matchup Evaluator for OpenWars")
    parser.add_argument("--mode", choices=["tui", "batch"], default="tui", help="Execution mode (tui or batch)")
    parser.add_argument("--map", default="map_3", help="Map(s) to test (comma separated, e.g. map_1,map_2,map_3)")
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
            
    maps = [mn.strip() for mn in args.map.split(",") if mn.strip()]

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
                for map_name in maps:
                    for i in range(args.games):
                        res = run_single_game(map_name, args.p1, args.p2, args.max_turns, lambda e: ui_callback_tui(e, layout, live))
                        res["map"] = map_name
                        res["p1"] = args.p1
                        res["p2"] = args.p2
                        all_results.append(res)

                        res2 = run_single_game(map_name, args.p2, args.p1, args.max_turns, lambda e: ui_callback_tui(e, layout, live))
                        res2["map"] = map_name
                        res2["p1"] = args.p2
                        res2["p2"] = args.p1
                        all_results.append(res2)
        else:
            print(json.dumps({"type": "info", "msg": f"Starting batch run: {args.p1} vs {args.p2} on {maps} ({args.games} games x 2 orders per map)"}))
            for map_name in maps:
                for i in range(args.games):
                    res = run_single_game(map_name, args.p1, args.p2, args.max_turns, ui_callback_batch)
                    res["map"] = map_name
                    res["p1"] = args.p1
                    res["p2"] = args.p2
                    all_results.append(res)
                    print(json.dumps({"type": "result", "data": {k: v for k, v in res.items() if k != "metrics" and k != "final_state"}}))

                    res2 = run_single_game(map_name, args.p2, args.p1, args.max_turns, ui_callback_batch)
                    res2["map"] = map_name
                    res2["p1"] = args.p2
                    res2["p2"] = args.p1
                    all_results.append(res2)
                    print(json.dumps({"type": "result", "data": {k: v for k, v in res2.items() if k != "metrics" and k != "final_state"}}))
                
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
