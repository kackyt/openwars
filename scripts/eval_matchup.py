import argparse
import subprocess
import json
import os
import sys
import time
import re
from collections import defaultdict
from pathlib import Path

if __package__:
    from scripts.eval_issue58 import (
        analyze_issue58_game,
        check_no_decline,
        collect_run_metadata,
        compare_issue58_baseline,
        generate_issue58_report,
        judge_issue58_criteria,
        parse_seed_list,
        validate_issue58_run,
        write_json_atomic,
        write_text_atomic,
    )
else:
    # 直接実行時は別 checkout の scripts パッケージではなく同階層を読む。
    from eval_issue58 import (
        analyze_issue58_game,
        check_no_decline,
        collect_run_metadata,
        compare_issue58_baseline,
        generate_issue58_report,
        judge_issue58_criteria,
        parse_seed_list,
        validate_issue58_run,
        write_json_atomic,
        write_text_atomic,
    )

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
    if os.name == 'nt':
        os.system('taskkill /F /IM mcp-server.exe >nul 2>&1')
    else:
        os.system('pkill -f mcp-server >/dev/null 2>&1')
    # スクリプト位置基準でリポジトリルートの実行ファイルを絶対パス解決する
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe_name = 'mcp-server.exe' if os.name == 'nt' else 'mcp-server'
    exe_path = os.path.join(repo_root, 'target', 'release', exe_name)
    p = subprocess.Popen(
        [exe_path],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        # 通常評価では画面を崩さない。停止診断時だけ親へ流し、サーバー側で
        # 最後に開始したAI stepを観測できるようにする。
        stderr=(
            None
            if os.environ.get("OPENWARS_TRACE_AI_STEPS")
            else subprocess.DEVNULL
        ),
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


ISSUE58_V3_V1 = "v3-v1"
ISSUE58_V3_SELFPLAY = "v3-selfplay"


def normalize_ai_version(version):
    """CLI で許容する AI バージョン表記を MCP が受け取る正規表記へ揃える。"""
    normalized = version.strip().upper()
    if normalized not in {"V1", "V2", "V3", "V4", "V100", "V200"}:
        raise ValueError(
            f"Invalid AI version: {version}. Expected one of V1, V2, V3, V4, V100 or V200"
        )
    return normalized


def build_match_specs(
    maps, subject, baseline, seeds, grid_type="hex", player_order="both"
):
    """各 seed を両方の手番へ決定的な順序で割り当てる。"""
    specs = []
    for map_name in maps:
        for seed in seeds:
            if player_order in {"both", "as-given"}:
                specs.append({
                    "map": map_name,
                    "p1": subject,
                    "p2": baseline,
                    "seed": seed,
                    "grid_type": grid_type,
                })
            if player_order in {"both", "swapped"}:
                specs.append({
                    "map": map_name,
                    "p1": baseline,
                    "p2": subject,
                    "seed": seed,
                    "grid_type": grid_type,
                })
    return specs


def build_issue58_match_specs(protocol, maps, seeds, grid_type="hex"):
    """Issue #58の固定評価プロトコルを決定的な順序で構築する。"""
    if protocol == ISSUE58_V3_V1:
        return build_match_specs(maps, "V3", "V1", seeds, grid_type=grid_type)
    if protocol == ISSUE58_V3_SELFPLAY:
        return [
            {
                "map": map_name,
                "p1": "V3",
                "p2": "V3",
                "seed": seed,
                "grid_type": grid_type,
            }
            for map_name in maps
            for seed in seeds
        ]
    raise ValueError(f"unknown Issue #58 protocol: {protocol}")


def write_trace_jsonl(path, results):
    """遊兵・生産・封鎖解除トレースを「1手番1行」の JSONL として書き出す。

    集計も判定もここでは行わない。engine が出した値をそのまま残すだけで、
    遊兵数の推移や「同一ユニットが全施設へ発注される」現象は
    この生ログを後から読み解いて確認する。
    """
    lines = []
    for game_index, result in enumerate(results):
        header = {
            "game_index": game_index,
            "map": result.get("map"),
            "p1": result.get("p1"),
            "p2": result.get("p2"),
            "seed": result.get("seed"),
        }
        # 1ラウンドにP1/P2の2手番が入るので (ラウンド, プレイヤー) で1行にまとめる。
        records = {}

        def record_for(entry):
            key = (entry.get("round"), entry.get("player_id"))
            record = records.get(key)
            if record is None:
                record = dict(header)
                record["round"] = entry.get("round")
                record["turn"] = entry.get("turn")
                record["player_id"] = entry.get("player_id")
                records[key] = record
            return record

        for entry in result.get("idle_audit_history", []):
            record_for(entry)["idle_audit"] = entry.get("audit")
        for entry in result.get("operation_assignment_history", []):
            record_for(entry)["operation_assignments"] = entry.get("assignments")
        for entry in result.get("island_campaign_history", []):
            record = record_for(entry)
            record["available_funds"] = entry.get("available_funds")
            record["island_campaign"] = entry.get("campaign")
        for entry in result.get("production_plan_history", []):
            record_for(entry)["production_plan"] = entry.get("plan")
        for entry in result.get("deployment_audit_history", []):
            record_for(entry)["deployment_audit"] = entry.get("audit")
        for entry in result.get("plan_revision_history", []):
            record_for(entry)["plan_revisions"] = entry.get("revisions")
        for entry in result.get("plan_execution_history", []):
            record_for(entry)["plan_executions"] = entry.get("executions")
        for entry in result.get("victory_roadmap_history", []):
            record_for(entry)["victory_roadmap"] = entry.get("roadmap")
        for entry in result.get("logistics_plan_history", []):
            record_for(entry)["logistics_plan"] = entry.get("plan")
        for entry in result.get("emergency_plan_history", []):
            record_for(entry)["emergency_plan"] = entry.get("plan")
        for entry in result.get("factory_relief_history", []):
            record_for(entry)["factory_relief"] = entry.get("missions")
        for entry in result.get("action_history", []):
            record_for(entry)["actions"] = entry.get("actions")

        # 欠測（V1〜V3 は生産トレースを持たない）を挟んでも順序が崩れないよう -1 で埋める。
        for key in sorted(
            records,
            key=lambda k: (
                k[0] if k[0] is not None else -1,
                k[1] if k[1] is not None else -1,
            ),
        ):
            lines.append(json.dumps(records[key], ensure_ascii=False))

    # 親ディレクトリ作成と置換は write_text_atomic 側が担保する。
    write_text_atomic(path, "".join(f"{line}\n" for line in lines))


def collect_round_metrics(tool, state, round_number):
    """両プレイヤーの行動が完了したラウンド末盤面を評価する。"""
    metric = {
        "turn": round_number,
        "p1_props": 0,
        "p2_props": 0,
        "p1_units": 0,
        "p2_units": 0,
        "p1_funds": 0,
        "p2_funds": 0,
        "p1_score": 0,
        "p2_score": 0,
    }
    for player_id, side in ((1, "p1"), (2, "p2")):
        evaluation = tool("evaluate_board", {"player_id": player_id})
        if not isinstance(evaluation, dict):
            evaluation = {}
        metric[f"{side}_score"] = evaluation.get("score", 0)
        metric[f"{side}_subj"] = evaluation.get("subjective_metrics", {})
        metric[f"{side}_obj"] = evaluation.get("objective_metrics", {})
    for player in state.get("players", []):
        player_id = player.get("player_id")
        if player_id not in (1, 2):
            continue
        side = "p1" if player_id == 1 else "p2"
        metric[f"{side}_props"] = player.get("property_count", 0)
        metric[f"{side}_units"] = player.get("unit_cost", 0)
        metric[f"{side}_funds"] = player.get("funds", 0)
        metric[f"{side}_abs_score"] = (
            metric[f"{side}_props"] * 20000 + metric[f"{side}_units"]
        )
    return metric


def run_single_game(
    map_name,
    p1_ver,
    p2_ver,
    max_turns,
    seed=None,
    grid_type="hex",
    ui_callback=None,
    tool_caller=None,
):
    # 計画書の `--p1 v4` のような小文字指定も、MCP の受け付ける大文字表記へ正規化する。
    p1_ver = normalize_ai_version(p1_ver)
    p2_ver = normalize_ai_version(p2_ver)
    tool = tool_caller or call_tool
    if ui_callback: ui_callback({"type": "log", "msg": f"Match started: P1({p1_ver}) vs P2({p2_ver}) on {map_name} ({grid_type})"})
    
    load_args = {"map_name": map_name, "grid_type": grid_type}
    if seed is not None:
        load_args["seed"] = seed
    tool("load_map", load_args)
    tool("set_player_ai_version", {"player_id": 1, "version": p1_ver})
    tool("set_player_ai_version", {"player_id": 2, "version": p2_ver})

    thinking_times = {1: [], 2: []}
    action_counts = {1: defaultdict(int), 2: defaultdict(int)}
    metrics = []
    invasion_events = []
    transport_history = []
    strategic_history = []
    island_campaign_history = []
    # 遊兵・生産判断・生産Entityの任務実績をターン別に保持する。
    # いずれも engine 側が判定済みの結果で、ここでは記録だけを行う。
    idle_audit_history = []
    operation_assignment_history = []
    production_plan_history = []
    deployment_audit_history = []
    plan_revision_history = []
    plan_execution_history = []
    victory_roadmap_history = []
    logistics_plan_history = []
    emergency_plan_history = []
    factory_relief_history = []
    action_history = []
    initial_state = None
    error = None

    def finish_game(result, turns, final_state):
        return {
            "result": result,
            "turns": turns,
            "seed": seed,
            "grid_type": grid_type,
            "thinking_times": thinking_times,
            "action_counts": action_counts,
            "metrics": metrics,
            "final_state": final_state,
            "initial_state": initial_state,
            "invasion_events": invasion_events,
            "transport_history": transport_history,
            "strategic_history": strategic_history,
            "island_campaign_history": island_campaign_history,
            "idle_audit_history": idle_audit_history,
            "operation_assignment_history": operation_assignment_history,
            "production_plan_history": production_plan_history,
            "deployment_audit_history": deployment_audit_history,
            "plan_revision_history": plan_revision_history,
            "plan_execution_history": plan_execution_history,
            "victory_roadmap_history": victory_roadmap_history,
            "logistics_plan_history": logistics_plan_history,
            "emergency_plan_history": emergency_plan_history,
            "factory_relief_history": factory_relief_history,
            "action_history": action_history,
            "error": error,
        }

    def record_round_state(round_state, round_number):
        metric = collect_round_metrics(tool, round_state, round_number)
        metrics.append(metric)
        strategic_history.append({
            "turn": round_number,
            "properties": round_state.get("properties", []),
            "units": round_state.get("units", []),
            "transport_squads": round_state.get("transport_squads", []),
        })
        if ui_callback:
            ui_callback({
                "type": "status_update",
                "metrics": metric,
                "p1_ver": p1_ver,
                "p2_ver": p2_ver,
            })

    turn = 1
    while turn <= max_turns:
        
        state = tool("get_board_state")
        if isinstance(state, dict) and state.get("error"):
            error = str(state["error"])
            if ui_callback: ui_callback({"type": "log", "msg": f"Error: {error}"})
            break
        if initial_state is None:
            initial_state = state

        game_over = state.get("game_over")
        if game_over:
            status = game_over.get("status")
            if status == "winner":
                winner_id = game_over.get("winner_id")
                winner_ver = p1_ver if winner_id == 1 else p2_ver
                if ui_callback: ui_callback({"type": "log", "msg": f"Game Finished! Winner: P{winner_id} ({winner_ver})"})
                return finish_game(f"P{winner_id}_Win", turn, state)
            elif status == "draw":
                if ui_callback: ui_callback({"type": "log", "msg": f"Game Finished! Draw"})
                return finish_game("Draw", turn, state)

        active_idx = state.get("active_player_index", 0)
        if ui_callback and active_idx == 0:
            ui_callback({"type": "turn_start", "turn": turn, "max_turns": max_turns})

        current_player = state["players"][active_idx]["player_id"]
        
        t0 = time.time()
        ai_result = tool("simulate_ai_turn")
        t1 = time.time()
        
        thinking_ms = (t1 - t0) * 1000
        thinking_times[current_player].append(thinking_ms)
        
        if isinstance(ai_result, dict) and ai_result.get("error"):
            error = str(ai_result["error"])
            if ui_callback: ui_callback({"type": "log", "msg": f"AI Error: {error}"})
            break

        campaign = ai_result.get("island_campaign")
        if campaign is not None:
            player_state = next(
                (
                    player
                    for player in state.get("players", [])
                    if player.get("player_id") == current_player
                ),
                {},
            )
            # 予算検証に必要な手番開始時の資金と自軍assetを診断snapshotへ添える。
            island_campaign_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "available_funds": player_state.get("funds", 0),
                    "units": [
                        unit
                        for unit in state.get("units", [])
                        if unit.get("player_id") == current_player
                    ],
                    "campaign": campaign,
                }
            )

        # 1ラウンドにP1/P2の2手番が入るため、必ずplayer_idを添えて区別する。
        idle_audit = ai_result.get("idle_audit")
        if idle_audit is not None:
            idle_audit_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "audit": idle_audit,
                }
            )

        operation_assignments = ai_result.get("operation_assignments")
        if operation_assignments is not None:
            operation_assignment_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "assignments": operation_assignments,
                }
            )

        production_plan = ai_result.get("production_plan")
        if production_plan is not None:
            production_plan_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "plan": production_plan,
                }
            )

        deployment_audit = ai_result.get("deployment_audit")
        if deployment_audit is not None:
            deployment_audit_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "audit": deployment_audit,
                }
            )

        plan_revisions = ai_result.get("plan_revisions")
        if plan_revisions is not None:
            plan_revision_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "revisions": plan_revisions,
                }
            )

        plan_executions = ai_result.get("plan_executions")
        if plan_executions is not None:
            plan_execution_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "executions": plan_executions,
                }
            )

        victory_roadmap = ai_result.get("victory_roadmap")
        if victory_roadmap is not None:
            victory_roadmap_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "roadmap": victory_roadmap,
                }
            )

        logistics_plan = ai_result.get("logistics_plan")
        if logistics_plan is not None:
            logistics_plan_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "plan": logistics_plan,
                }
            )

        emergency_plan = ai_result.get("emergency_plan")
        if emergency_plan is not None:
            emergency_plan_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "plan": emergency_plan,
                }
            )

        factory_relief = ai_result.get("factory_relief")
        if factory_relief:
            factory_relief_history.append(
                {
                    "round": turn,
                    "turn": state.get("turn"),
                    "player_id": ai_result.get("player_id", current_player),
                    "missions": factory_relief,
                }
            )

        invasion_events.extend(ai_result.get("invasion_events", []))
        transport_history.append({
            "turn": turn,
            "player_id": current_player,
            "squads": ai_result.get("transport_squads", []),
        })
        actions = ai_result.get("actions_taken", [])
        # V100/V200のROM行動列と座標単位で比較できるよう、集計前の命令文字列も残す。
        action_history.append(
            {
                "round": turn,
                "turn": state.get("turn"),
                "player_id": current_player,
                "actions": [str(action) for action in actions],
            }
        )
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
            # AiCommand (V2/V3 系) のデバッグ文字列。Capture は座標付きで記録し、
            # どの拠点を占領しようとしているか追跡できるようにする
            elif action_str.startswith("Capture"):
                m2 = re.search(r"x:\s*(\d+),\s*y:\s*(\d+)", action_str)
                if m2:
                    acts_dict[f"Capture@({m2.group(1)},{m2.group(2)})"] += 1
                else:
                    acts_dict["Capture"] += 1
            elif action_str.startswith("Attack"):
                acts_dict["Attack"] += 1
            elif action_str.startswith("Wait"):
                acts_dict["Wait"] += 1
            elif action_str.startswith("Merge"):
                acts_dict["Merge"] += 1
            elif action_str.startswith("Load"):
                acts_dict["Load"] += 1
            elif action_str.startswith("Drop"):
                acts_dict["Drop"] += 1
            elif action_str.startswith("Supply"):
                acts_dict["Supply"] += 1

        if ui_callback and acts_dict:
            act_str = ", ".join([f"{k}({v})" for k, v in acts_dict.items()])
            ui_callback({"type": "log", "msg": f"P{current_player} T{turn}: {act_str}"})

        post_action_state = tool("get_board_state")
        post_game_over = post_action_state.get("game_over")
        if post_game_over:
            # 決着時は手番の途中でも、その行動が反映された盤面を最終値にする。
            record_round_state(post_action_state, turn)
            status = post_game_over.get("status")
            if status == "winner":
                winner_id = post_game_over.get("winner_id")
                return finish_game(f"P{winner_id}_Win", turn, post_action_state)
            return finish_game("Draw", turn, post_action_state)

        if active_idx == 1:
            # P1/P2 の双方が行動した後だけ、同じラウンド番号の盤面を保存する。
            state = post_action_state
            record_round_state(state, turn)
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
            winner_ver = p1_ver if winner_id == 1 else p2_ver
            ui_callback({"type": "log", "msg": f"Game Finished! Winner: P{winner_id} ({winner_ver}) (Absolute Score: {p1_final} vs {p2_final})"})
        else:
            ui_callback({"type": "log", "msg": f"Game Finished! Draw (Absolute Score: {p1_final} vs {p2_final})"})

    return finish_game(result_str, max_turns, state)

def analyze_issue54_game(game, subject="V3", stall_turns=5):
    """同一カーゴの搭載・敵初期島上陸・侵攻成立と輸送停滞を判定する。"""
    if game.get("p1") == subject:
        subject_player = 1
        order = "先攻"
    elif game.get("p2") == subject:
        subject_player = 2
        order = "後攻"
    else:
        return None
    enemy_player = 2 if subject_player == 1 else 1
    initial_state = game.get("initial_state") or {}
    enemy_capital_islands = {
        prop.get("island_id")
        for prop in initial_state.get("properties", [])
        if str(prop.get("terrain_type", prop.get("terrain", ""))).lower() == "capital"
        and prop.get("owner") == enemy_player
        and prop.get("island_id") is not None
    }

    loads = {}
    landings = {}
    invasion_cargo = set()
    evidence = []
    for event in game.get("invasion_events", []):
        event_type = event.get("type")
        if event_type == "unit_loaded" and event.get("player_id") == subject_player:
            loads[event.get("cargo_id")] = event
        elif event_type == "unit_unloaded" and event.get("player_id") == subject_player:
            cargo_id = event.get("cargo_id")
            loaded = loads.get(cargo_id)
            if (
                loaded
                and loaded.get("transport_id") == event.get("transport_id")
                and loaded.get("island_id") != event.get("island_id")
                and event.get("island_id") in enemy_capital_islands
            ):
                landings[cargo_id] = event
                evidence.append({
                    "cargo_id": cargo_id,
                    "transport_id": event.get("transport_id"),
                    "load_turn": loaded.get("turn"),
                    "unload_turn": event.get("turn"),
                    "unload_position": [event.get("x"), event.get("y")],
                    "interaction": None,
                })
        elif event_type == "unit_attacked":
            for cargo_id in (event.get("attacker_id"), event.get("defender_id")):
                if cargo_id in landings:
                    invasion_cargo.add(cargo_id)
                    for item in evidence:
                        if item["cargo_id"] == cargo_id and item["interaction"] is None:
                            role = "attacker" if event.get("attacker_id") == cargo_id else "defender"
                            item["interaction"] = f"attack:{role}@T{event.get('turn')}"
        elif event_type == "property_capture_progressed":
            cargo_id = event.get("unit_id")
            if cargo_id in landings:
                invasion_cargo.add(cargo_id)
                for item in evidence:
                    if item["cargo_id"] == cargo_id and item["interaction"] is None:
                        status = "completed" if event.get("completed") else "started"
                        item["interaction"] = f"capture:{status}@T{event.get('turn')}"

    safety_violations = []
    stall_state = {}
    reported_stalls = set()
    for record in game.get("transport_history", []):
        if record.get("player_id") != subject_player:
            continue
        for squad in record.get("squads", []):
            if squad.get("player_id") != subject_player:
                continue
            transport_id = squad.get("transport_id")
            planned = tuple(squad.get("planned_cargo_ids", []))
            loaded = tuple(squad.get("loaded_cargo_ids", []))
            phase = squad.get("phase")
            if phase == "Return" and (planned or loaded):
                safety_violations.append(
                    f"transport {transport_id} entered Return with cargo planned={planned} loaded={loaded}"
                )
            if phase not in ("Transit", "Drop"):
                stall_state.pop(transport_id, None)
                continue
            signature = (phase, squad.get("x"), squad.get("y"), planned, loaded)
            previous_signature, count = stall_state.get(transport_id, (None, 0))
            count = count + 1 if signature == previous_signature else 1
            stall_state[transport_id] = (signature, count)
            if count >= stall_turns and transport_id not in reported_stalls:
                safety_violations.append(
                    f"transport {transport_id} stalled in {phase} for {count} subject turns"
                )
                reported_stalls.add(transport_id)

    return {
        "map": game.get("map"),
        "order": order,
        "subject_player": subject_player,
        "landing": bool(landings),
        "invasion": bool(invasion_cargo),
        "safety_violations": safety_violations,
        "evidence": evidence,
    }


def judge_issue54_criteria(results, subject="V3", stall_turns=5):
    """#54の合否を手番別に集約する。勝率・経済・決着ターンは使用しない。"""
    analyses = [
        analysis
        for analysis in (
            analyze_issue54_game(game, subject, stall_turns) for game in results
        )
        if analysis is not None
    ]
    buckets = defaultdict(lambda: {"games": 0, "landings": 0, "invasions": 0, "violations": []})
    for analysis in analyses:
        bucket = buckets[(analysis["map"], analysis["order"])]
        bucket["games"] += 1
        bucket["landings"] += int(analysis["landing"])
        bucket["invasions"] += int(analysis["invasion"])
        bucket["violations"].extend(analysis["safety_violations"])

    rows = []
    for (map_name, order), bucket in sorted(buckets.items()):
        passed = (
            bucket["landings"] >= 1
            and bucket["invasions"] >= 1
            and not bucket["violations"]
        )
        rows.append({"map": map_name, "order": order, "passed": passed, **bucket})

    maps = {game.get("map") for game in results}
    expected = {(map_name, order) for map_name in maps for order in ("先攻", "後攻")}
    observed = set(buckets)
    overall = expected == observed and bool(rows) and all(row["passed"] for row in rows)
    return overall, rows, analyses


def generate_issue54_report(results, subject="V3", baseline="V2", seed=None, stall_turns=5):
    overall, rows, analyses = judge_issue54_criteria(results, subject, stall_turns)
    report = [
        "# Issue #54 島嶼侵攻評価レポート",
        "",
        "## 合否判定",
        "",
        "敵初期島への上陸と、同一カーゴによる攻撃・被攻撃・占領開始/完了のみを判定します。",
        "勝率、経済指標、最終スコア、30ターン以内の決着は参考情報であり、合否には使用しません。",
        f"- 対象AI: {subject} / 比較AI: {baseline}",
        f"- seed: {seed if seed is not None else '未指定'}",
        f"- 停滞閾値: {stall_turns} 自軍ターン",
        "",
        "| マップ | 手番 | 試合数 | 敵初期島上陸 | 侵攻成立 | 安全性違反 | 判定 |",
        "| :--- | :--- | ---: | ---: | ---: | ---: | :--- |",
    ]
    for row in rows:
        report.append(
            f"| {row['map']} | {row['order']} | {row['games']} | {row['landings']} | "
            f"{row['invasions']} | {len(row['violations'])} | "
            f"{'**PASS**' if row['passed'] else '**FAIL**'} |"
        )
    report.extend(["", f"**全体判定: {'PASS' if overall else 'FAIL'}**", "", "## 試合別証跡", ""])
    for index, (game, analysis) in enumerate(zip(results, analyses), start=1):
        report.append(
            f"### Game {index}: {game.get('map')} P1({game.get('p1')}) vs P2({game.get('p2')})"
        )
        report.append(f"- 結果（参考）: {game.get('result')} / {game.get('turns')}ターン")
        report.append(f"- {subject}手番: {analysis['order']}")
        if analysis["evidence"]:
            for item in analysis["evidence"]:
                report.append(
                    f"- cargo {item['cargo_id']} / transport {item['transport_id']}: "
                    f"Load T{item['load_turn']} → Unload T{item['unload_turn']} "
                    f"at {tuple(item['unload_position'])} → {item['interaction'] or 'interactionなし'}"
                )
        else:
            report.append("- 敵初期島への上陸証跡なし")
        for violation in analysis["safety_violations"]:
            report.append(f"- 安全性違反: {violation}")
        report.append("")
    return "\n".join(report), overall


def judge_objective_criteria(results, subject="V2", baseline="V1"):
    """確定済みの客観メトリクス基準で合否判定する。
    subject = 評価対象の新AI、baseline = 比較対象の旧AI。
    基準1: 判定時点(30T or 決着時点)の ZOC支配面積 subject平均 > baseline平均（先攻・後攻それぞれ）
    基準2: 同・ターン収入
    基準3: subjectのユニット資産価値・収入の5T移動平均が15T以降に減少トレンドへ転じない
           （ZOCは終盤のユニット密集で重複減少し誤検知するため、ストック指標で判定する。
            ZOCの優位性自体は基準1でカバーされる）
    戻り値: (per_map判定dict, 全体PASS/FAIL, 詳細行リスト)"""
    # (map, order) -> 集計
    buckets = defaultdict(lambda: {"v2_zoc": [], "v1_zoc": [], "v2_inc": [], "v1_inc": [], "trend_ok": []})

    for g in results:
        p1, p2 = g["p1"], g["p2"]
        if subject not in (p1, p2) or p1 == p2:
            continue
        v2_side = "p1" if p1 == subject else "p2"
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


def generate_report(results, subject="V2", baseline="V1"):
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
            if p1 == subject: v2_wins += 1; v2_win_turns.append(game["turns"])
            else: v1_wins += 1; v1_win_turns.append(game["turns"])
        elif "P2_Win" in res:
            if p2 == subject: v2_wins += 1; v2_win_turns.append(game["turns"])
            else: v1_wins += 1; v1_win_turns.append(game["turns"])
        else:
            draws += 1

        t1 = game.get("thinking_times", {}).get(1, [])
        t2 = game.get("thinking_times", {}).get(2, [])
        if p1 == subject: thinking_times_v2.extend(t1)
        else: thinking_times_v1.extend(t1)
        if p2 == subject: thinking_times_v2.extend(t2)
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
    map_pass, overall, detail_rows = judge_objective_criteria(results, subject, baseline)
    report.append("## ✅ 合否判定（客観メトリクス基準）")
    report.append("判定時点 = 各戦の30ターン時点（それ以前に決着した場合は決着時点）。")
    report.append("")
    report.append(f"| マップ | 手番 | 基準1: ZOC支配面積 ({subject} vs {baseline}) | 基準2: ターン収入 ({subject} vs {baseline}) | 基準3: ジリ貧解消 | 判定 |")
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
    report.append(f"- **{subject} (新AI) の総合勝率（参考・ガードレール40%）**: **{v2_win_rate:.1f}%** ({v2_wins}勝 {v1_wins}敗 {draws}分)")
    report.append(f"- **平均勝利ターン数**: ")
    report.append(f"  - **{subject} (新AI) 勝利時**: {avg_turns_v2:.1f} ターン")
    report.append(f"  - **{baseline} (旧AI) 勝利時**: {avg_turns_v1:.1f} ターン")
    report.append(f"- **平均思考時間 (1ターンあたり)**: ")
    report.append(f"  - **{subject} (新AI)**: **{avg_time_v2:.1f} ms**")
    report.append(f"  - **{baseline} (旧AI)**: **{avg_time_v1:.1f} ms**\n")
    
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
    parser.add_argument(
        "--player-order",
        choices=["both", "as-given", "swapped"],
        default="both",
        help="Run both player orders or only one order (default: both)",
    )
    parser.add_argument("--max-turns", type=int, default=30, help="Maximum turns per game")
    parser.add_argument("--criteria", choices=["objective", "issue54", "issue58"], default="objective", help="Acceptance criteria used for the final report")
    parser.add_argument(
        "--issue58-protocol",
        choices=[ISSUE58_V3_V1, ISSUE58_V3_SELFPLAY],
        help="Fixed Issue #58 evaluation protocol",
    )
    parser.add_argument(
        "--artifact-stage",
        choices=["baseline", "result"],
        help="Whether this run captures the pre-change baseline or post-change result",
    )
    parser.add_argument("--seed", type=int, default=None, help="Deterministic game RNG seed")
    parser.add_argument("--seeds", default=None, help="Comma-separated deterministic seed set")
    parser.add_argument("--json-output", default=None, help="Raw JSON output path")
    parser.add_argument(
        "--trace-output",
        default=None,
        help="JSONL path for per-turn idle-unit and production traces",
    )
    parser.add_argument(
        "--baseline-json",
        default=None,
        help="Baseline JSON artifact required for Issue #58 result runs",
    )
    parser.add_argument(
        "--grid-type",
        "--map-type",
        choices=["square", "hex"],
        default="hex",
        help="Grid topology for maps (square or hex, default: hex)",
    )
    parser.add_argument("--stall-turns", type=int, default=5, help="Subject turns before an unchanged transport is considered stalled")
    parser.add_argument("--output", default="matchup_report.md", help="Output file for the final report")
    args = parser.parse_args()

    maps = tuple(mn.strip() for mn in args.map.split(",") if mn.strip())
    issue58_metadata = None
    baseline_payload = None
    if args.criteria == "issue58":
        if args.player_order != "both":
            parser.error("Issue #58 fixed protocol requires --player-order both")
        if not args.issue58_protocol:
            parser.error("Issue #58 requires --issue58-protocol")
        if not args.artifact_stage:
            parser.error("Issue #58 requires --artifact-stage")
        if not args.seeds:
            parser.error("Issue #58 requires --seeds")
        if not args.json_output:
            parser.error("Issue #58 requires --json-output")
        try:
            seeds = parse_seed_list(args.seeds)
            validate_issue58_run(
                args.issue58_protocol,
                args.artifact_stage,
                maps,
                args.p1,
                args.p2,
                args.max_turns,
                seeds,
                args.output,
                args.json_output,
                args.baseline_json,
            )
            match_specs = build_issue58_match_specs(
                args.issue58_protocol,
                maps,
                seeds,
                grid_type=args.grid_type,
            )
        except ValueError as error:
            parser.error(str(error))
        issue58_metadata = collect_run_metadata(
            sys.argv.copy(),
            seeds,
            ("scripts/eval_matchup.py", "scripts/eval_issue58.py"),
            ("mcp-server/src/main.rs", "mcp-server/src/invasion_trace.rs"),
        )
        # 固定プロトコルの再現性を成果物だけで検証できるように記録する。
        issue58_metadata.update(
            {
                "protocol": args.issue58_protocol,
                "artifact_stage": args.artifact_stage,
                "expected_games": len(match_specs),
                "games_per_seed": len(match_specs) // len(seeds),
                "subject": args.p1,
                "baseline": args.p2,
                "artifact_path": str(Path(args.json_output).resolve()),
                "baseline_artifact_path": (
                    str(Path(args.baseline_json).resolve())
                    if args.baseline_json
                    else None
                ),
            }
        )
        if args.artifact_stage == "result":
            try:
                baseline_path = Path(args.baseline_json).resolve()
                baseline_payload = json.loads(
                    baseline_path.read_text(encoding="utf-8")
                )
                baseline_metadata = baseline_payload.setdefault("metadata", {})
                # Task 1 artifactはartifact_path追加前に取得済みのため、指定した実ファイルを
                # in-memory metadataへ補い、原本JSON自体は変更しない。
                baseline_metadata.setdefault("artifact_path", str(baseline_path))
                recorded_baseline_path = Path(
                    baseline_metadata.get("artifact_path", "")
                ).resolve()
                if recorded_baseline_path != baseline_path:
                    raise ValueError(
                        "baseline artifact_path does not match --baseline-json"
                    )
                if baseline_payload.get("results"):
                    # 旧self-play baselineの単側analysisをraw resultsから両側へ再計算し、
                    # 同じmap/player-orderのthinking-time分布を比較可能にする。
                    baseline_payload["analyses"] = [
                        analysis
                        for result in baseline_payload["results"]
                        for analysis in analyze_issue58_game(
                            result, args.issue58_protocol
                        )
                    ]
                # 試合を開始する前にprotocol・seed・hash・成果物識別子の不一致を拒否する。
                compare_issue58_baseline(
                    {"metadata": issue58_metadata, "analyses": []},
                    baseline_payload,
                )
            except (OSError, json.JSONDecodeError, ValueError) as error:
                parser.error(str(error))
    else:
        seeds = tuple(args.seed for _ in range(args.games))
        match_specs = build_match_specs(
            maps,
            args.p1,
            args.p2,
            seeds,
            grid_type=args.grid_type,
            player_order=args.player_order,
        )

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
            p1_ver = event.get("p1_ver", args.p1)
            p2_ver = event.get("p2_ver", args.p2)
            t.add_row(f"P1 ({p1_ver})", str(m["p1_funds"]), str(m["p1_props"]), str(m["p1_units"]), str(m.get("p1_score", 0)), str(m.get("p1_abs_score", 0)))
            t.add_row(f"P2 ({p2_ver})", str(m["p2_funds"]), str(m["p2_props"]), str(m["p2_units"]), str(m.get("p2_score", 0)), str(m.get("p2_abs_score", 0)))
            layout["status"].update(Panel(t, title=f"Turn {m['turn']}"))
        
        live.refresh()

    def ui_callback_batch(event):
        if event["type"] == "log":
            print(f"[LOG] {event['msg']}")
        elif event["type"] == "status_update":
            m = event["metrics"]
            print(json.dumps({"type": "metrics", "data": m}))
            
    criteria_pass = True
    execution_incomplete = False

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
                for spec in match_specs:
                    result = run_single_game(
                        spec["map"],
                        spec["p1"],
                        spec["p2"],
                        args.max_turns,
                        seed=spec["seed"],
                        grid_type=spec.get("grid_type", args.grid_type),
                        ui_callback=lambda e: ui_callback_tui(e, layout, live),
                    )
                    result.update(spec)
                    all_results.append(result)
        else:
            print(json.dumps({"type": "info", "msg": f"Starting batch run: {args.p1} vs {args.p2} on {maps} ({len(match_specs)} matches, order={args.player_order})"}))
            for spec in match_specs:
                result = run_single_game(
                    spec["map"],
                    spec["p1"],
                    spec["p2"],
                    args.max_turns,
                    seed=spec["seed"],
                    grid_type=spec.get("grid_type", args.grid_type),
                    ui_callback=ui_callback_batch,
                )
                result.update(spec)
                all_results.append(result)
                print(json.dumps({
                    "type": "result",
                    "data": {
                        key: value
                        for key, value in result.items()
                        if key not in {
                            "metrics",
                            "final_state",
                            "initial_state",
                            "invasion_events",
                            "transport_history",
                            "strategic_history",
                            "island_campaign_history",
                            "idle_audit_history",
                            "operation_assignment_history",
                            "production_plan_history",
                            "deployment_audit_history",
                            "plan_revision_history",
                            "plan_execution_history",
                            "emergency_plan_history",
                            "factory_relief_history",
                        }
                    },
                }))

        if args.trace_output:
            write_trace_jsonl(args.trace_output, all_results)
            if args.mode == "batch":
                print(json.dumps({"type": "info", "msg": f"Trace written to {args.trace_output}"}))

        if args.criteria == "issue58":
            analyses = [
                analysis
                for result in all_results
                for analysis in analyze_issue58_game(
                    result, args.issue58_protocol
                )
            ]
            criteria_pass, criteria_rows, summaries = judge_issue58_criteria(
                all_results,
                protocol=args.issue58_protocol,
            )
            baseline_comparison = []
            payload = {
                "metadata": issue58_metadata,
                "overall_pass": criteria_pass,
                "criteria_rows": criteria_rows,
                "summaries": summaries,
                "analyses": analyses,
                "baseline_comparison": baseline_comparison,
                "results": all_results,
            }
            if baseline_payload is not None:
                baseline_comparison = compare_issue58_baseline(payload, baseline_payload)
                payload["baseline_comparison"] = baseline_comparison
                criteria_pass = criteria_pass and all(
                    row.get("thinking_time_pass") for row in baseline_comparison
                )
                payload["overall_pass"] = criteria_pass

            # 実行失敗と受け入れ基準を分離し、baseline は基準未達でも必ず保存する。
            runtime_incomplete = (
                len(all_results) != len(match_specs)
                or any(result.get("error") for result in all_results)
            )
            expected_analysis_count = len(match_specs) * (
                2 if args.issue58_protocol == ISSUE58_V3_SELFPLAY else 1
            )
            criteria_incomplete = (
                len(analyses) != expected_analysis_count
                or any(not row.get("complete") for row in criteria_rows)
            )
            execution_incomplete = runtime_incomplete or (
                args.artifact_stage == "result" and criteria_incomplete
            )
            report = generate_issue58_report(payload)
            # JSON を原本として先に確定し、同じ payload から Markdown を生成する。
            write_json_atomic(args.json_output, payload)
            write_text_atomic(args.output, report)
        elif args.criteria == "issue54":
            report, criteria_pass = generate_issue54_report(
                all_results,
                subject=args.p1,
                baseline=args.p2,
                seed=args.seed,
                stall_turns=args.stall_turns,
            )
            with open(args.output, "w", encoding="utf-8") as file:
                file.write(report)
        else:
            report = generate_report(all_results, subject=args.p1, baseline=args.p2)
            with open(args.output, "w", encoding="utf-8") as file:
                file.write(report)

        if args.mode == "batch":
            print(json.dumps({"type": "info", "msg": f"Report generated at {args.output}"}))

    finally:
        if p:
            p.stdin.close()
            p.wait()

    if args.mode == "batch" and args.criteria == "issue58":
        if execution_incomplete:
            raise SystemExit(2)
        if args.artifact_stage == "result" and not criteria_pass:
            raise SystemExit(1)
    if args.mode == "batch" and args.criteria == "issue54" and not criteria_pass:
        raise SystemExit(1)

if __name__ == "__main__":
    main()
