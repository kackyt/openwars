import subprocess
import json
import os
import time

env = os.environ.copy()
env['RUST_LOG'] = 'trace'

# Ensure the process is not locked
os.system('taskkill /F /IM mcp-server.exe')

p = subprocess.Popen(
    ['target/debug/mcp-server.exe'],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    encoding='utf-8',
    env=env
)

def send(req):
    line = json.dumps(req)
    p.stdin.write(line + '\n')
    p.stdin.flush()

def receive():
    line = p.stdout.readline()
    if not line:
        return None
    return json.loads(line)

# Initialize
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "1.0.0"}}})
receive()
send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})

# 1. Load map
send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "load_map", "arguments": {"map_name": "map_1"}}})
print("Load Map:", receive()['result']['content'][0]['text'])

# 2. Get initial state
send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_board_state", "arguments": {}}})
state_initial = json.loads(receive()['result']['content'][0]['text'])
print(f"Initial Units: {state_initial['units']}")

# 3. Advance to Player 2
# Turn 1 Main P1 -> (next_phase) -> Turn 1 Main P2
print("Advancing to P2 (AI)...")
send({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "execute_action", "arguments": {"action_type": "next_phase"}}})
print(receive()['result']['content'][0]['text'])

# 4. Simulate AI turn for P2 (Loop until turn ends)
print("Simulating AI turn (P2)...")
send({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "simulate_ai_turn", "arguments": {}}})
res = receive()
data = json.loads(res['result']['content'][0]['text'])
print(f"  Actions: {data['actions_taken']}")
# 5. Get final state
send({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "get_board_state", "arguments": {}}})
state_final = json.loads(receive()['result']['content'][0]['text'])
print(f"Final Units: {state_final['units']}")

p.stdin.close()
p.wait()
