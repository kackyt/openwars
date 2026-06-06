import subprocess
import json
import os
import sys
import time
from collections import defaultdict

# 定数定義
MAX_TURNS = 30  # 1ゲームの最大ターン数
MAPS_TO_TEST = ["map_1", "map_2", "map_3"]  # 評価に使用するマップ一覧
GAMES_PER_MATCHUP = 1  # 各先攻後攻の組み合わせでの対戦数（合計: マップ数 * 2パターン * N回）

# 環境変数の設定
env = os.environ.copy()
env['RUST_LOG'] = 'info'

# すでに実行中のmcp-serverプロセスがあれば強制終了する
os.system('taskkill /F /IM mcp-server.exe >nul 2>&1')

# MCP サーバープロセスの起動
p = subprocess.Popen(
    ['target/release/mcp-server.exe'],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=sys.stderr,
    text=True,
    encoding='utf-8',
    env=env
)

def send_request(method, params=None, req_id=1):
    """MCPサーバーにJSON-RPCリクエストを送信します。"""
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
    """MCPサーバーからJSON-RPCレスポンスを受信します。"""
    line = p.stdout.readline()
    if not line:
        return None
    return json.loads(line)

def call_tool(name, arguments=None, req_id=1):
    """MCPツールの呼び出しをラップしたヘルパー関数。"""
    params = {
        "name": name,
        "arguments": arguments or {}
    }
    send_request("tools/call", params, req_id)
    res = receive_response()
    if not res:
        raise Exception(f"No response from tool: {name}")
    if "error" in res:
        raise Exception(f"Tool {name} returned error: {res['error']}")
    
    content = res['result']['content'][0]['text']
    try:
        return json.loads(content)
    except json.JSONDecodeError:
        return content

# 1. MCP初期化シーケンス
send_request("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "ai-matchup-evaluator", "version": "1.0.0"}}, 0)
receive_response()
# send_request("notifications/initialized", None)

def run_single_game(map_name, p1_version, p2_version):
    """指定されたマップとAIの組み合わせで1ゲーム実行し、結果を辞書で返します。"""
    print(f"  [Game Start] Map: {map_name} | Player 1 ({p1_version}) vs Player 2 ({p2_version})")
    
    # マップのロード
    call_tool("load_map", {"map_name": map_name})
    
    # プレイヤーのAIバージョンを設定
    call_tool("set_player_ai_version", {"player_id": 1, "version": p1_version})
    call_tool("set_player_ai_version", {"player_id": 2, "version": p2_version})
    
    turn = 1
    total_thinking_time = defaultdict(float)
    turn_counts = defaultdict(int)
    
    while turn <= MAX_TURNS:
        # 現在の状態を取得
        state = call_tool("get_board_state")
        
        # 勝敗決着チェック
        game_over = state.get("game_over")
        if game_over:
            status = game_over.get("status")
            if status == "winner":
                winner_id = game_over.get("winner_id")
                print(f"    -> Game Finished on Turn {turn}! Winner: Player {winner_id}")
                return {
                    "result": f"Player {winner_id}",
                    "winner_id": winner_id,
                    "turns": turn,
                    "thinking_time_ms": dict(total_thinking_time),
                    "turn_counts": dict(turn_counts),
                    "final_state": state
                }
            elif status == "draw":
                print(f"    -> Game Finished on Turn {turn} (Draw)!")
                return {
                    "result": "Draw",
                    "winner_id": None,
                    "turns": turn,
                    "thinking_time_ms": dict(total_thinking_time),
                    "turn_counts": dict(turn_counts),
                    "final_state": state
                }
        
        # AIターンのシミュレート
        start_time = time.time()
        ai_result = call_tool("simulate_ai_turn")
        end_time = time.time()
        
        # 思考時間の記録
        active_idx = state["active_player_index"]
        player_id = state["players"][active_idx]["player_id"]
        elapsed = (end_time - start_time) * 1000  # ミリ秒
        
        total_thinking_time[player_id] += elapsed
        turn_counts[player_id] += 1
        
        
        # アクションの出力（V2のスタック問題を調査するため）
        if player_id == 2 or True:
            actions = ai_result.get("actions_taken", [])
            print(f"    Actions taken: {len(actions)}")
            if len(actions) > 0 and len(actions) < 10:
                print(f"    {actions}")
            elif len(actions) >= 10:
                print(f"    {actions[:5]} ... and {len(actions)-5} more")
            
        print(f"turn: {turn} ({player_id}) end {elapsed} ms")
        # 状態を再取得してターン数を更新
        next_state = call_tool("get_board_state")
        turn = next_state["turn"]
    
    # ターン上限に達した場合は、状態価値スコア（拠点数 * 2000 + ユニットコスト、所持金無視）が高い方を暫定勝者とする
    print("    -> Turn limit reached. Resolving by board state value...")
    state = call_tool("get_board_state")
    p1 = state["players"][0]
    p2 = state["players"][1]
    
    p1_props = p1.get("property_count", 0)
    p2_props = p2.get("property_count", 0)
    p1_unit_cost = p1.get("unit_cost", 0)
    p2_unit_cost = p2.get("unit_cost", 0)
    
    p1_score = p1_props * 20000 + p1_unit_cost
    p2_score = p2_props * 20000 + p2_unit_cost
    
    winner_id = 1 if p1_score > p2_score else (2 if p2_score > p1_score else None)
    winner_str = f"Player {winner_id} (State Value Decision)" if winner_id else "Draw"
    
    print(f"    -> Winner by Board Value: {winner_str} (P1 Score: {p1_score} [Props: {p1_props}, Units: {p1_unit_cost}] vs P2 Score: {p2_score} [Props: {p2_props}, Units: {p2_unit_cost}])")
    
    return {
        "result": winner_str,
        "winner_id": winner_id,
        "turns": MAX_TURNS,
        "thinking_time_ms": dict(total_thinking_time),
        "turn_counts": dict(turn_counts),
        "final_state": state
    }

def generate_report(results):
    """収集した対戦結果から、美しく整理された markdown レポートを生成します。"""
    report = []
    report.append("# 🤖 AI 対戦評価レポート (Matchup Report)\n")
    report.append(f"**生成日時**: {time.strftime('%Y-%m-%d %H:%M:%S')}\n")
    report.append("このレポートは、新AI（V2：部隊システム＋ビーム探索）と旧AI（V1：貪欲法）の直接対戦結果をまとめ、その実力差を多角的に評価したものです。\n")
    
    # 総合結果集計
    total_games = 0
    v2_wins = 0
    v1_wins = 0
    draws = 0
    
    # ターン数と時間
    v2_win_turns = []
    v1_win_turns = []
    
    thinking_times_v1 = []
    thinking_times_v2 = []
    
    map_summaries = defaultdict(list)
    
    for game in results:
        total_games += 1
        map_name = game["map"]
        p1_ver = game["p1"]
        p2_ver = game["p2"]
        winner_id = game["winner_id"]
        turns = game["turns"]
        
        # 思考時間の集計
        for pid, t_time in game["thinking_time_ms"].items():
            ver = p1_ver if pid == 1 else p2_ver
            player_turns = game["turn_counts"].get(pid, 0)
            if player_turns == 0:
                continue
            if ver == "V1":
                thinking_times_v1.append(t_time / player_turns)
            else:
                thinking_times_v2.append(t_time / player_turns)
        
        # 勝者の判定
        v2_is_winner = False
        v1_is_winner = False
        if winner_id == 1:
            v2_is_winner = (p1_ver == "V2")
            v1_is_winner = (p1_ver == "V1")
        elif winner_id == 2:
            v2_is_winner = (p2_ver == "V2")
            v1_is_winner = (p2_ver == "V1")
            
        if v2_is_winner:
            v2_wins += 1
            v2_win_turns.append(turns)
            game_res = "V2 勝利"
        elif v1_is_winner:
            v1_wins += 1
            v1_win_turns.append(turns)
            game_res = "V1 勝利"
        else:
            draws += 1
            game_res = "引き分け"
            
        map_summaries[map_name].append({
            "matchup": f"P1({p1_ver}) vs P2({p2_ver})",
            "result": game_res,
            "turns": turns,
            "p1_funds": game["final_state"]["players"][0]["funds"],
            "p2_funds": game["final_state"]["players"][1]["funds"],
            "p1_props": game["final_state"]["players"][0].get("property_count", 0),
            "p2_props": game["final_state"]["players"][1].get("property_count", 0),
        })
        
    v2_win_rate = (v2_wins / total_games) * 100 if total_games > 0 else 0
    avg_turns_v2 = sum(v2_win_turns) / len(v2_win_turns) if v2_win_turns else 0
    avg_turns_v1 = sum(v1_win_turns) / len(v1_win_turns) if v1_win_turns else 0
    avg_time_v2 = sum(thinking_times_v2) / len(thinking_times_v2) if thinking_times_v2 else 0
    avg_time_v1 = sum(thinking_times_v1) / len(thinking_times_v1) if thinking_times_v1 else 0
    
    # サマリーセクション
    report.append("## 📊 総合結果サマリー")
    report.append(f"- **総対戦数**: {total_games} ゲーム")
    report.append(f"- **V2 (新AI) の総合勝率**: **{v2_win_rate:.1f}%** ({v2_wins}勝 {v1_wins}敗 {draws}分)")
    report.append(f"- **平均勝利ターン数**: ")
    report.append(f"  - **V2 (新AI) 勝利時**: {avg_turns_v2:.1f} ターン")
    report.append(f"  - **V1 (旧AI) 勝利時**: {avg_turns_v1:.1f} ターン")
    report.append(f"- **平均思考時間 (1ターンあたり)**: ")
    report.append(f"  - **V2 (新AI)**: **{avg_time_v2:.1f} ms**")
    report.append(f"  - **V1 (旧AI)**: **{avg_time_v1:.1f} ms**\n")
    
    # 勝率の評価グラフ風表現
    bar_len = 20
    v2_bar = "█" * int(bar_len * (v2_wins / total_games))
    v1_bar = "░" * int(bar_len * (v1_wins / total_games))
    draw_bar = "▒" * int(bar_len * (draws / total_games))
    report.append(f"```\n勝率推移: [ {v2_bar}{draw_bar}{v1_bar} ]\n          新AI V2 ({v2_wins}) | 引き分け ({draws}) | 旧AI V1 ({v1_wins})\n```\n")

    # マップ別セクション
    report.append("## 🗺️ マップ別対戦詳細")
    for map_name, games in map_summaries.items():
        report.append(f"### 📍 {map_name}")
        report.append("| 対戦カード | 結果 | ターン数 | P1 最終資金 | P2 最終資金 | P1 最終拠点数 | P2 最終拠点数 |")
        report.append("| :--- | :--- | :--- | :--- | :--- | :--- | :--- |")
        for g in games:
            report.append(f"| {g['matchup']} | **{g['result']}** | {g['turns']} | {g['p1_funds']}G | {g['p2_funds']}G | {g['p1_props']} | {g['p2_props']} |")
        report.append("\n")
        
    report.append("## 📝 評価考察")
    if v2_win_rate > 60.0:
        report.append("> 🎉 **新AI（V2）の強さの実証に成功しました！**\n> 部隊（Squad）による連携移動と、ビーム探索による目標割り当て最適化により、従来の貪欲法（V1）を勝率で大幅に上回っています。特に勝利時の平均ターン数が短縮されており、無駄のない速攻や包囲網形成が実現できていることが示唆されます。")
    elif v2_win_rate >= 45.0:
        report.append("> ⚖️ **新旧AIの実力は拮抗しています。**\n> V2は部隊連携を行っていますが、思考アルゴリズムの調整や、探索の評価関数のパラメータチューニング（弾薬補正・孤立補正などの重み付け）に改善の余地があります。")
    else:
        report.append("> ⚠️ **新AI（V2）の勝率が旧AIを下回るか、改善が見られません。**\n> 部隊の編成ルール（SquadPlanner）が粗いか、ビーム探索のロールアウト評価が正確に機能していない可能性があります。まずはSoloFallback時の挙動や、脅威判定の閾値を再評価することをお勧めします。")
        
    return "\n".join(report)

# 対戦実行
all_results = []
try:
    for map_name in MAPS_TO_TEST:
        print(f"\n--- Testing Map: {map_name} ---")
        
        # パターン1: P1(V2) vs P2(V1)
        for i in range(GAMES_PER_MATCHUP):
            res = run_single_game(map_name, "V2", "V1")
            res["map"] = map_name
            res["p1"] = "V2"
            res["p2"] = "V1"
            all_results.append(res)
            
        # パターン2: P1(V1) vs P2(V2)
        for i in range(GAMES_PER_MATCHUP):
            res = run_single_game(map_name, "V1", "V2")
            res["map"] = map_name
            res["p1"] = "V1"
            res["p2"] = "V2"
            all_results.append(res)

    # レポート生成と書き出し
    print("\n--- Generating Matchup Report ---")
    markdown_report = generate_report(all_results)
    
    with open("matchup_report.md", "w", encoding="utf-8") as f:
        f.write(markdown_report)
        
    print("Matchup report successfully generated and saved to: matchup_report.md")

finally:
    # 接続クローズとプロセスの終了
    p.stdin.close()
    p.wait()
    print("\nEvaluator finished. MCP server stopped.")
