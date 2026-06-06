import subprocess
import json
import os
import sys
import time
from collections import defaultdict

# 定数定義
MAX_TURNS = 10  # デバッグのため、まずは10ターン実行
MAP_NAME = "map_2"

# 環境変数の設定
env = os.environ.copy()
env['RUST_LOG'] = 'info'

# すでに実行中のmcp-serverプロセスがあれば強制終了する
os.system('taskkill /F /IM mcp-server.exe >nul 2>&1')

print("Starting mcp-server.exe for debugging V2 AI strategy...")

# MCP サーバープロセスの起動
# stderr=sys.stderr にすることで、Rust側の eprintln! 出力がそのまま Python のコンソールに流れます。
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
send_request("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "v2-ai-debugger", "version": "1.0.0"}}, 0)
receive_response()

try:
    print(f"\n=== [Debug Session Start] Map: {MAP_NAME} ===")
    
    # マップのロード
    print("Loading map...")
    call_tool("load_map", {"map_name": MAP_NAME})
    
    # プレイヤーのAIバージョンを設定
    # P1 (Red): V1 AI (旧AI・貪欲法)
    # P2 (Blue): V2 AI (新AI)
    print("Setting Player AI versions (P1: V1, P2: V2)...")
    call_tool("set_player_ai_version", {"player_id": 1, "version": "V1"})
    call_tool("set_player_ai_version", {"player_id": 2, "version": "V2"})
    
    turn = 1
    
    while turn <= MAX_TURNS:
        # 現在の状態を取得
        state = call_tool("get_board_state")
        
        # 勝敗決着チェック
        game_over = state.get("game_over")
        if game_over:
            status = game_over.get("status")
            if status == "winner":
                winner_id = game_over.get("winner_id")
                print(f"\n🎉 Game Finished on Turn {turn}! Winner: Player {winner_id}")
                break
            elif status == "draw":
                print(f"\n🤝 Game Finished on Turn {turn} (Draw)!")
                break
        
        active_idx = state["active_player_index"]
        active_player = state["players"][active_idx]
        player_id = active_player["player_id"]
        ai_version = "V2" if player_id == 1 else "V1"
        
        print(f"\n==============================================")
        print(f" 🚩 TURN {turn} | Player {player_id} ({ai_version}) Phase: {state['phase']}")
        print(f"    Funds: {active_player['funds']}G | Properties: {active_player['property_count']} | Unit Cost: {active_player['unit_cost']}G")
        print(f"==============================================")
        
        # AIターンのシミュレート
        print(f"Thinking...")
        start_time = time.time()
        result = call_tool("simulate_ai_turn")
        end_time = time.time()
        
        elapsed = (end_time - start_time) * 1000  # ミリ秒
        
        print(f"\n--- [Turn Result Summary] ---")
        print(f"Thinking time: {elapsed:.1f} ms")
        print(f"Board Score: {result.get('before_score')} -> {result.get('after_score')}")
        print(f"Actions Taken:")
        
        actions = result.get("actions_taken", [])
        if actions:
            for idx, action in enumerate(actions, 1):
                print(f"  {idx}. {action}")
        else:
            print("  No actions taken.")
            
        # 状態を再取得してターン数を更新
        next_state = call_tool("get_board_state")
        turn = next_state["turn"]

    print("\n=== [Debug Session End] ===")

finally:
    # 接続クローズとプロセスの終了
    p.stdin.close()
    p.wait()
    print("\nDebugger finished. MCP server stopped.")
