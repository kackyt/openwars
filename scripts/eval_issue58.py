from __future__ import annotations

import hashlib
import json
import math
import os
import statistics
import subprocess
from collections import defaultdict
from pathlib import Path


def parse_seed_list(raw: str) -> tuple[int, ...]:
    seeds = tuple(int(part.strip()) for part in raw.split(",") if part.strip())
    if len(set(seeds)) != len(seeds):
        raise ValueError("each seed must be unique")
    return seeds


def validate_issue58_run(
    protocol: str,
    artifact_stage: str,
    maps: tuple[str, ...],
    subject: str,
    baseline: str,
    max_turns: int,
    seeds: tuple[int, ...],
    markdown_output: str,
    json_output: str,
    baseline_json: str | None = None,
) -> None:
    """Issue #58の固定プロトコルと成果物出力先を検証する。"""
    if protocol == "v3-v1":
        if maps != ("map_1", "map_2", "map_3"):
            raise ValueError("Issue #58 v3-v1 requires map_1,map_2,map_3")
        if subject != "V3" or baseline != "V1":
            raise ValueError("Issue #58 v3-v1 requires V3 versus V1")
    elif protocol == "v3-selfplay":
        if maps != ("map_3",):
            raise ValueError("Issue #58 v3-selfplay is map_3 only")
        if subject != "V3" or baseline != "V3":
            raise ValueError("Issue #58 v3-selfplay requires V3 versus V3")
    else:
        raise ValueError(f"unknown Issue #58 protocol: {protocol}")

    if artifact_stage not in {"baseline", "result"}:
        raise ValueError("artifact_stage must be baseline or result")
    if artifact_stage == "result" and not baseline_json:
        raise ValueError("artifact_stage=result requires --baseline-json")
    if max_turns != 30:
        raise ValueError("Issue #58 requires max_turns=30")
    if seeds != (58001, 58002, 58003, 58004):
        raise ValueError("Issue #58 requires seeds 58001,58002,58003,58004")
    if Path(markdown_output).name == "matchup_report.md":
        raise ValueError("Issue #58 must not overwrite matchup_report.md")
    if Path(markdown_output).resolve() == Path(json_output).resolve():
        raise ValueError("Markdown and JSON outputs must be different files")


def sha256_files(paths: tuple[str, ...]) -> str:
    digest = hashlib.sha256()
    # 入力順に依存せず、同じファイル集合なら同じ評価ツール識別子にする。
    for raw_path in sorted(paths):
        path = Path(raw_path)
        digest.update(path.as_posix().encode("utf-8"))
        digest.update(path.read_bytes())
    return digest.hexdigest()


def collect_run_metadata(
    argv: list[str],
    seeds: tuple[int, ...],
    evaluator_paths: tuple[str, ...],
    mcp_paths: tuple[str, ...],
) -> dict:
    commit_sha = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    dirty = bool(
        subprocess.run(
            ["git", "status", "--porcelain"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    )
    return {
        "commit_sha": commit_sha,
        "working_tree_dirty": dirty,
        "command": argv,
        "seeds": list(seeds),
        "games_per_order": len(seeds),
        "evaluator_sha256": sha256_files(evaluator_paths),
        "mcp_sha256": sha256_files(mcp_paths),
    }


def moving_average(values: list[float], window: int = 5) -> list[float]:
    averages = []
    for index in range(len(values)):
        current = values[max(0, index - window + 1) : index + 1]
        averages.append(sum(current) / len(current))
    return averages


def check_no_decline(series: list[float], start_turn: int = 15) -> bool:
    """T15 以降の5ターン移動平均が基準点を下回らないことを確認する。"""
    if len(series) < start_turn:
        return True
    averages = moving_average(series)
    return averages[-1] >= averages[start_turn - 1]


def subject_won(game: dict, subject_player: int) -> bool:
    result = str(game.get("result", ""))
    return result.startswith(f"P{subject_player}_Win")


def production_investment(events: list[dict], player_id: int) -> dict[str, int]:
    investment: defaultdict[str, int] = defaultdict(int)
    for event in events:
        if event.get("type") != "unit_produced" or event.get("player_id") != player_id:
            continue
        unit_type = event.get("unit_type")
        if unit_type:
            investment[str(unit_type)] += int(event.get("cost", 0))
    return dict(sorted(investment.items()))


def combat_value_by_unit_type(events: list[dict], player_id: int) -> dict[str, int]:
    combat_value: defaultdict[str, int] = defaultdict(int)
    for event in events:
        if (
            event.get("type") != "unit_attacked"
            or event.get("attacker_player_id") != player_id
        ):
            continue
        unit_type = event.get("attacker_unit_type")
        if unit_type:
            combat_value[str(unit_type)] += int(event.get("damage_value_dealt", 0))
    return dict(sorted(combat_value.items()))


def first_event_turn(
    events: list[dict], event_type: str, player_id: int
) -> int | None:
    turns = [
        int(event["turn"])
        for event in events
        if event.get("type") == event_type
        and event.get("player_id") == player_id
        and event.get("turn") is not None
    ]
    return min(turns) if turns else None


def _property_key(prop: dict) -> tuple[int | None, int | None, int | None]:
    return (prop.get("island_id"), prop.get("x"), prop.get("y"))


def _external_properties(
    state: dict, player_id: int, initial_islands: set[int]
) -> set[tuple[int | None, int | None, int | None]]:
    return {
        _property_key(prop)
        for prop in state.get("properties", [])
        if prop.get("owner") == player_id
        and prop.get("island_id") not in initial_islands
    }


def _capital_distances(
    history: list[dict],
    player_id: int,
    capture_unit_ids: set[int],
    target_islands: set[int],
    enemy_capitals: list[dict],
) -> list[int]:
    distances = []
    for snapshot in sorted(history, key=lambda item: item.get("turn", 0)):
        round_distances = []
        for unit in snapshot.get("units", []):
            if (
                unit.get("unit_id") not in capture_unit_ids
                or unit.get("player_id") != player_id
                or unit.get("island_id") not in target_islands
            ):
                continue
            for capital in enemy_capitals:
                if capital.get("island_id") != unit.get("island_id"):
                    continue
                round_distances.append(
                    abs(int(unit.get("x", 0)) - int(capital.get("x", 0)))
                    + abs(int(unit.get("y", 0)) - int(capital.get("y", 0)))
                )
        if round_distances:
            distances.append(min(round_distances))
    return distances


def _analyze_issue58_player_metrics(game: dict, subject_player: int) -> dict:
    if subject_player not in (1, 2):
        raise ValueError(f"subject_player must be 1 or 2: {subject_player}")
    baseline_player = 2 if subject_player == 1 else 1
    subject_side = "p1" if subject_player == 1 else "p2"
    baseline_side = "p2" if subject_player == 1 else "p1"
    order = "先攻" if subject_player == 1 else "後攻"

    initial_state = game.get("initial_state") or {}
    final_state = game.get("final_state") or {}
    events = game.get("invasion_events") or []
    history = game.get("strategic_history") or []
    metrics_by_turn = {
        metric.get("turn"): metric
        for metric in game.get("metrics", [])
        if metric.get("turn") is not None
    }
    metrics = [metrics_by_turn[turn] for turn in sorted(metrics_by_turn)]
    last_metric = metrics[-1] if metrics else {}
    subject_objective = last_metric.get(f"{subject_side}_obj", {})
    baseline_objective = last_metric.get(f"{baseline_side}_obj", {})

    initial_islands = {
        prop.get("island_id")
        for prop in initial_state.get("properties", [])
        if prop.get("owner") == subject_player and prop.get("island_id") is not None
    }
    final_external = _external_properties(final_state, subject_player, initial_islands)
    acquired_external = set()
    for snapshot in history:
        acquired_external.update(
            _external_properties(snapshot, subject_player, initial_islands)
        )

    subject_events = [event for event in events if event.get("player_id") == subject_player]
    capture_events = [
        event
        for event in subject_events
        if event.get("type") == "property_capture_progressed"
        and event.get("island_id") is not None
        and event.get("island_id") not in initial_islands
    ]
    capture_started = {
        (event.get("unit_id"), event.get("x"), event.get("y"))
        for event in capture_events
    }
    capture_completed = {
        (event.get("unit_id"), event.get("x"), event.get("y"))
        for event in capture_events
        if event.get("completed")
    }

    landed_capture_events = [
        event
        for event in subject_events
        if event.get("type") == "unit_unloaded" and event.get("can_capture")
    ]
    landed_capture_ids = {
        event.get("cargo_id")
        for event in landed_capture_events
        if event.get("cargo_id") is not None
    }
    unload_turns = {
        event.get("cargo_id"): int(event.get("turn", 0))
        for event in landed_capture_events
    }
    first_capture_turns: dict[int, int] = {}
    completion_turns: dict[int, int] = {}
    for event in capture_events:
        unit_id = event.get("unit_id")
        if unit_id not in landed_capture_ids:
            continue
        event_turn = int(event.get("turn", 0))
        first_capture_turns[unit_id] = min(
            first_capture_turns.get(unit_id, event_turn), event_turn
        )
        if event.get("completed"):
            completion_turns[unit_id] = min(
                completion_turns.get(unit_id, event_turn), event_turn
            )
    capture_delays = [
        first_capture_turns[unit_id] - unload_turns[unit_id]
        for unit_id in first_capture_turns
        if unit_id in unload_turns
    ]
    landing_to_capture_turns = min(capture_delays) if capture_delays else None

    final_unit_ids = {
        unit.get("unit_id")
        for unit in final_state.get("units", [])
        if unit.get("player_id") == subject_player
    }
    history_last_seen: dict[int, int] = {}
    for snapshot in history:
        snapshot_turn = int(snapshot.get("turn", 0))
        for unit in snapshot.get("units", []):
            if unit.get("player_id") == subject_player:
                history_last_seen[unit.get("unit_id")] = snapshot_turn
    survived_capture_units = 0
    for unit_id in landed_capture_ids:
        required_turn = completion_turns.get(unit_id)
        if unit_id in final_unit_ids or (
            required_turn is not None
            and history_last_seen.get(unit_id, -1) >= required_turn
        ):
            survived_capture_units += 1
    capture_unit_survival_rate = (
        survived_capture_units / len(landed_capture_ids)
        if landed_capture_ids
        else None
    )

    investment = production_investment(events, subject_player)
    combat_value = combat_value_by_unit_type(events, subject_player)
    invasion_transport_names = {
        "Lander",
        "TransportHelicopter",
        "Transport Helicopter",
        "輸送船",
        "輸送ヘリ",
    }
    produced_transports = [
        event
        for event in subject_events
        if event.get("type") == "unit_produced"
        and event.get("unit_type") in invasion_transport_names
    ]
    transport_capacity_produced = sum(
        int(event.get("max_cargo", 0)) for event in produced_transports
    )

    target_islands = {
        event.get("island_id")
        for event in landed_capture_events
        if event.get("island_id") is not None
    }
    enemy_capitals = [
        prop
        for prop in initial_state.get("properties", [])
        if prop.get("owner") == baseline_player
        and str(prop.get("terrain_type", prop.get("terrain", ""))).lower()
        == "capital"
    ]
    capital_distances = _capital_distances(
        history,
        subject_player,
        landed_capture_ids,
        target_islands,
        enemy_capitals,
    )

    engagement_turns = [
        int(event["turn"])
        for event in events
        if event.get("type") == "unit_attacked"
        and (
            event.get("attacker_player_id") == subject_player
            or event.get("defender_player_id") == subject_player
        )
        and event.get("turn") is not None
    ]
    completed_capture_turns = [
        int(event["turn"])
        for event in capture_events
        if event.get("completed") and event.get("turn") is not None
    ]
    capture_start_turns = [
        int(event["turn"])
        for event in capture_events
        if event.get("turn") is not None
    ]
    milestones = {
        "first_transport_production": min(
            (int(event["turn"]) for event in produced_transports), default=None
        ),
        "first_load": first_event_turn(events, "unit_loaded", subject_player),
        "first_drop": first_event_turn(events, "unit_unloaded", subject_player),
        "first_combat": min(engagement_turns) if engagement_turns else None,
        "first_capture_start": min(capture_start_turns)
        if capture_start_turns
        else None,
        "first_capture_complete": min(completed_capture_turns)
        if completed_capture_turns
        else None,
    }

    asset_series = [metric.get(f"{subject_side}_units", 0) for metric in metrics]
    income_series = [
        metric.get(f"{subject_side}_obj", {}).get("income_per_turn", 0)
        for metric in metrics
    ]
    thinking_times = game.get("thinking_times", {})
    subject_thinking_times = thinking_times.get(
        subject_player, thinking_times.get(str(subject_player), [])
    )
    battleship_names = {"Battleship", "戦艦"}
    battleship_investment = sum(
        cost for unit_type, cost in investment.items() if unit_type in battleship_names
    )
    battleship_damage = sum(
        value
        for unit_type, value in combat_value.items()
        if unit_type in battleship_names
    )
    completed_external_captures = sorted(
        (event for event in capture_events if event.get("completed")),
        key=lambda event: int(event.get("turn", 0)),
    )
    first_external_property_capture = (
        {
            "turn": completed_external_captures[0].get("turn"),
            "island_id": completed_external_captures[0].get("island_id"),
            "x": completed_external_captures[0].get("x"),
            "y": completed_external_captures[0].get("y"),
        }
        if completed_external_captures
        else None
    )

    return {
        "map": game.get("map"),
        "seed": game.get("seed"),
        "order": order,
        "subject_player": subject_player,
        "error": game.get("error"),
        "result": game.get("result"),
        "won": subject_won(game, subject_player),
        "metrics_complete": bool(metrics),
        "final_zoc": subject_objective.get("zoc_area", 0),
        "final_income": subject_objective.get("income_per_turn", 0),
        "final_properties": subject_objective.get("owned_properties", 0),
        "baseline_final_zoc": baseline_objective.get("zoc_area", 0),
        "baseline_final_income": baseline_objective.get("income_per_turn", 0),
        "baseline_final_properties": baseline_objective.get("owned_properties", 0),
        "external_properties_gained": len(final_external),
        "external_properties_retained": len(acquired_external & final_external),
        "external_properties_lost_after_capture": len(
            acquired_external - final_external
        ),
        "first_external_property_capture": first_external_property_capture,
        "landed_capture_units": len(landed_capture_ids),
        "capture_started": len(capture_started),
        "capture_completed": len(capture_completed),
        "capture_unit_survival_rate": capture_unit_survival_rate,
        "landing_to_capture_turns": landing_to_capture_turns,
        "capital_distance_first": capital_distances[0]
        if capital_distances
        else None,
        "capital_distance_min": min(capital_distances)
        if capital_distances
        else None,
        "milestones": milestones,
        "transport_capacity_produced": transport_capacity_produced,
        "production_investment": investment,
        "combat_value_by_unit_type": combat_value,
        "battleship_investment": battleship_investment,
        "battleship_damage_value": battleship_damage,
        "battleship_roi": battleship_damage / battleship_investment
        if battleship_investment
        else None,
        "asset_trend_ok": check_no_decline(asset_series),
        "income_trend_ok": check_no_decline(income_series),
        "thinking_ms": list(subject_thinking_times),
    }


def _campaign_records(game: dict, player_number: int) -> list[dict]:
    return sorted(
        [
            record
            for record in game.get("island_campaign_history", [])
            if record.get("player_id") == player_number
        ],
        key=lambda record: (
            int(record.get("round") or 0),
            int(record.get("turn") or 0),
        ),
    )


def _expected_island_ids(game: dict, records: list[dict]) -> set[int]:
    expected = {
        int(item["island_id"])
        for collection in ("properties", "units")
        for item in (game.get("initial_state") or {}).get(collection, [])
        if item.get("island_id") is not None
    }
    if records:
        expected.update(
            int(island["island_id"])
            for island in records[0].get("campaign", {}).get("islands", [])
            if island.get("island_id") is not None
        )
    return expected


def _assignment_entity_ids(assignment: dict) -> set[int]:
    return {
        int(entity_id)
        for field in (
            "transport_entity_ids",
            "capture_entity_ids",
            "combat_entity_ids",
        )
        for entity_id in assignment.get(field, [])
    }


def _known_unit_costs(game: dict, player_number: int) -> dict[int, int]:
    costs = {}
    snapshots = [game.get("initial_state") or {}, game.get("final_state") or {}]
    snapshots.extend(game.get("strategic_history", []))
    snapshots.extend(game.get("island_campaign_history", []))
    for snapshot in snapshots:
        for unit in snapshot.get("units", []):
            if (
                unit.get("unit_id") is not None
                and unit.get("player_id") == player_number
            ):
                costs[int(unit["unit_id"])] = int(unit.get("cost", 0))
    for event in game.get("invasion_events", []):
        if (
            event.get("type") == "unit_produced"
            and event.get("player_id") == player_number
            and event.get("unit_id") is not None
        ):
            costs[int(event["unit_id"])] = int(event.get("cost", 0))
    return costs


def analyze_issue58_player(
    game: dict, player_number: int, protocol: str
) -> dict:
    """1人のV3プレイヤーについて島ポートフォリオの行動とhard failureを解析する。"""
    analysis = _analyze_issue58_player_metrics(game, player_number)
    records = _campaign_records(game, player_number)
    expected_islands = _expected_island_ids(game, records)
    initial_owners_by_island: defaultdict[int, set[int | None]] = defaultdict(set)
    for prop in (game.get("initial_state") or {}).get("properties", []):
        if prop.get("island_id") is not None:
            initial_owners_by_island[int(prop["island_id"])].add(prop.get("owner"))
    initial_unit_islands = {
        int(unit["island_id"])
        for unit in (game.get("initial_state") or {}).get("units", [])
        if unit.get("island_id") is not None
    }
    initial_neutral_islands = {
        island_id
        for island_id, owners in initial_owners_by_island.items()
        if owners == {None} and island_id not in initial_unit_islands
    }
    hard_failures = []
    max_offensives = 0
    first_offensive = None
    first_offensive_set = []
    first_offensive_islands = []
    contested_reason_present = False
    secured_after_enemy_removal = False
    continued_secure = False
    threatened_preemption = False
    enemy_assault_budget_compliant = True
    previous_offensive_count = None
    known_unit_costs = _known_unit_costs(game, player_number)

    def fail(code: str, message: str, record: dict | None = None) -> None:
        hard_failures.append(
            {
                "code": code,
                "message": message,
                "round": None if record is None else record.get("round"),
                "turn": None if record is None else record.get("turn"),
            }
        )

    if game.get("error"):
        fail("game_error", str(game.get("error")))
    if not records:
        fail("missing_campaign_history", "V3 campaign telemetry was not recorded")

    for record in records:
        campaign = record.get("campaign") or {}
        islands = campaign.get("islands") or []
        assessments = {
            int(island["island_id"]): island
            for island in islands
            if island.get("island_id") is not None
        }
        missing = sorted(expected_islands - set(assessments))
        if missing:
            fail(
                "missing_island_state",
                f"missing island assessments: {missing}",
                record,
            )

        active_offensives = campaign.get("active_offensives") or []
        defenses = campaign.get("defenses") or []
        offensive_ids = {
            int(assignment["island_id"])
            for assignment in active_offensives
            if assignment.get("island_id") is not None
            and assignment.get("decision")
            in {"Expand", "Contest", "Reinforce", "Assault"}
        }
        max_offensives = max(max_offensives, len(offensive_ids))
        if len(offensive_ids) > 3:
            fail(
                "too_many_offensives",
                f"simultaneous offensive islands: {sorted(offensive_ids)}",
                record,
            )
        if first_offensive is None and active_offensives:
            first_offensive_islands = islands
            for assignment in active_offensives:
                island_id = assignment.get("island_id")
                assessment = (
                    assessments.get(int(island_id))
                    if island_id is not None
                    else None
                )
                first_offensive_set.append(
                    {
                        "round": record.get("round"),
                        "turn": record.get("turn"),
                        "island_id": island_id,
                        "decision": assignment.get("decision"),
                        "state": None if assessment is None else assessment.get("state"),
                        "preferred_transport": (
                            assignment.get("requirement") or {}
                        ).get("preferred_transport"),
                        "capture_units": (assignment.get("requirement") or {}).get(
                            "capture_units", 0
                        ),
                        "roi_production_sites": (
                            0
                            if assessment is None
                            else assessment.get("roi_production_sites", 0)
                        ),
                        "neutral_properties": (
                            0
                            if assessment is None
                            else assessment.get("neutral_properties", 0)
                        ),
                        "transport_eta": (
                            None
                            if assessment is None
                            else assessment.get("transport_eta")
                        ),
                        "expansion_payback_turns": (
                            None
                            if assessment is None
                            else assessment.get("expansion_payback_turns")
                        ),
                    }
                )
            first_offensive = first_offensive_set[0]

        assignments = [*defenses, *active_offensives]
        entity_assignment: dict[int, int] = {}
        assigned_entity_ids = set()
        for index, assignment in enumerate(assignments):
            for entity_id in _assignment_entity_ids(assignment):
                previous = entity_assignment.get(entity_id)
                if previous is not None and previous != index:
                    fail(
                        "duplicate_entity_assignment",
                        f"entity {entity_id} appears in assignments {previous} and {index}",
                        record,
                    )
                entity_assignment[entity_id] = index
                assigned_entity_ids.add(entity_id)

        available_resources = int(record.get("available_funds", 0)) + sum(
            known_unit_costs.get(entity_id, 0) for entity_id in assigned_entity_ids
        )
        allocated_budget = sum(
            int(assignment.get("allocated_budget", 0)) for assignment in assignments
        )
        if allocated_budget > available_resources:
            fail(
                "allocated_budget_exceeds_resources",
                f"allocated={allocated_budget} available={available_resources}",
                record,
            )

        for island_id, assessment in assessments.items():
            state = assessment.get("state")
            decision = assessment.get("decision")
            if state == "Contested" and assessment.get("decision_reason"):
                contested_reason_present = True
            if (
                state == "Secured"
                and int(assessment.get("enemy_properties", 0)) == 0
                and int(assessment.get("neutral_properties", 0)) > 0
            ):
                secured_after_enemy_removal = True
            if state == "Secured" and decision == "Secure":
                continued_secure = True
            if state == "Threatened" and (
                assessment.get("enemy_arrival_eta") is not None
                and int(assessment["enemy_arrival_eta"]) <= 2
            ):
                defended = any(
                    defense.get("island_id") == island_id for defense in defenses
                )
                preempted = (
                    previous_offensive_count is not None
                    and len(offensive_ids) < previous_offensive_count
                )
                threatened_preemption = threatened_preemption or defended or preempted
            if state == "EnemyHeld" and decision == "Assault":
                compliant = int(assessment.get("allocated_budget", 0)) >= int(
                    assessment.get("required_budget", 0)
                )
                enemy_assault_budget_compliant &= compliant
                if not compliant:
                    fail(
                        "underfunded_enemy_assault",
                        "EnemyHeld Assault budget is below required_budget",
                        record,
                    )
        previous_offensive_count = len(offensive_ids)

    initial_campaign_islands = (
        records[0].get("campaign", {}).get("islands", []) if records else []
    )
    first_states = {
        int(island["island_id"]): island.get("state")
        for island in initial_campaign_islands
        if island.get("island_id") is not None
    }
    initial_neutral_open = all(
        first_states.get(island_id) == "OpenNeutral"
        for island_id in initial_neutral_islands
    )
    first_offensive_open_neutral = bool(
        first_offensive
        and first_offensive.get("state") == "OpenNeutral"
        and first_offensive.get("decision") == "Expand"
    )
    ranked_open_neutral = sorted(
        (
            island
            for island in first_offensive_islands
            if island.get("state") == "OpenNeutral"
            and island.get("decision") == "Expand"
            and island.get("expansion_payback_turns") is not None
        ),
        key=lambda island: (
            int(island["expansion_payback_turns"]),
            -int(island.get("roi_production_sites", 0)),
            -int(island.get("neutral_properties", 0)),
            (
                int(island["transport_eta"])
                if island.get("transport_eta") is not None
                else float("inf")
            ),
            int(island["island_id"]),
        ),
    )
    expected_first_open_neutral_island_id = (
        int(ranked_open_neutral[0]["island_id"]) if ranked_open_neutral else None
    )
    first_offensive_roi_ranked = bool(
        first_offensive
        and first_offensive.get("state") == "OpenNeutral"
        and first_offensive.get("decision") == "Expand"
        and first_offensive.get("island_id") is not None
        and int(first_offensive["island_id"])
        == expected_first_open_neutral_island_id
    )
    first_roi_offensive = first_offensive if first_offensive_roi_ranked else None

    analysis.update(
        {
            "protocol": protocol,
            "campaign_turns": len(records),
            "expected_island_ids": sorted(expected_islands),
            "initial_neutral_open": initial_neutral_open,
            "first_offensive": first_offensive,
            "first_offensive_set": first_offensive_set,
            "expected_first_open_neutral_island_id": expected_first_open_neutral_island_id,
            "first_roi_offensive": first_roi_offensive,
            "first_offensive_open_neutral": first_offensive_open_neutral,
            "first_offensive_roi_ranked": first_offensive_roi_ranked,
            "max_simultaneous_offensives": max_offensives,
            "contested_reason_present": contested_reason_present,
            "secured_after_enemy_removal": secured_after_enemy_removal,
            "continued_secure": continued_secure,
            "threatened_preemption": threatened_preemption,
            "enemy_assault_budget_compliant": enemy_assault_budget_compliant,
            "hard_failures": hard_failures,
            "hard_failure_codes": list(
                dict.fromkeys(failure["code"] for failure in hard_failures)
            ),
            "behavior_pass": not hard_failures,
        }
    )
    return analysis


def analyze_issue58_game(game: dict, protocol: str) -> list[dict]:
    if protocol not in {"v3-v1", "v3-selfplay"}:
        raise ValueError(f"unknown Issue #58 protocol: {protocol}")
    players = [
        player_number
        for player_number, version in ((1, game.get("p1")), (2, game.get("p2")))
        if version == "V3"
    ]
    return [analyze_issue58_player(game, player, protocol) for player in players]


def average(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def percentile95(values: list[float]) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    # nearest-rank 法で入力件数が少ない固定 seed 集計でも決定的にする。
    index = max(0, math.ceil(len(ordered) * 0.95) - 1)
    return ordered[index]


def compare_issue58_baseline(
    result_payload: dict, baseline_payload: dict
) -> list[dict]:
    """同一protocol・map・手番のbaseline思考時間とresultを比較する。"""
    result_metadata = result_payload.get("metadata") or {}
    baseline_metadata = baseline_payload.get("metadata") or {}
    if result_metadata.get("protocol") != baseline_metadata.get("protocol"):
        raise ValueError("baseline protocol does not match result protocol")
    if result_metadata.get("seeds") != baseline_metadata.get("seeds"):
        raise ValueError("baseline seeds do not match result seeds")
    if baseline_metadata.get("artifact_stage") != "baseline":
        raise ValueError("baseline artifact_stage must be baseline")
    if result_metadata.get("artifact_stage") != "result":
        raise ValueError("result artifact_stage must be result")
    if result_metadata.get("expected_games") != baseline_metadata.get("expected_games"):
        raise ValueError("baseline expected game count does not match result")
    for label, metadata in (
        ("baseline", baseline_metadata),
        ("result", result_metadata),
    ):
        if not metadata.get("evaluator_sha256") or not metadata.get("mcp_sha256"):
            raise ValueError(f"{label} evaluator/MCP hashes are required")
        if not metadata.get("commit_sha") or not metadata.get("artifact_path"):
            raise ValueError(f"{label} commit SHA and artifact path are required")
    if result_metadata.get("commit_sha") == baseline_metadata.get("commit_sha"):
        raise ValueError("result commit SHA must differ from baseline")
    if result_metadata.get("artifact_path") == baseline_metadata.get("artifact_path"):
        raise ValueError("result artifact path must differ from baseline")

    def grouped_thinking(payload: dict) -> dict[tuple[str, str], list[float]]:
        grouped: defaultdict[tuple[str, str], list[float]] = defaultdict(list)
        for analysis in payload.get("analyses", []):
            key = (str(analysis.get("map")), str(analysis.get("order")))
            grouped[key].extend(float(value) for value in analysis.get("thinking_ms", []))
        return grouped

    result_groups = grouped_thinking(result_payload)
    baseline_groups = grouped_thinking(baseline_payload)
    rows = []
    for key in sorted(result_groups):
        if key not in baseline_groups:
            raise ValueError(f"baseline thinking-time bucket missing: {key}")
        result_mean = average(result_groups[key])
        baseline_mean = average(baseline_groups[key])
        thinking_ratio = (
            result_mean / baseline_mean
            if baseline_mean > 0
            else (1.0 if result_mean == 0 else float("inf"))
        )
        rows.append(
            {
                "map": key[0],
                "order": key[1],
                "result_thinking_mean_ms": result_mean,
                "baseline_thinking_mean_ms": baseline_mean,
                "thinking_ratio": thinking_ratio,
                "thinking_time_pass": thinking_ratio <= 1.5,
            }
        )
    return rows


def judge_issue58_criteria(
    results: list[dict],
    protocol: str = "v3-v1",
    baseline_payload: dict | None = None,
) -> tuple[bool, list[dict], list[dict]]:
    analyses = [
        analysis
        for game in results
        for analysis in analyze_issue58_game(game, protocol)
    ]
    buckets: defaultdict[tuple[str | None, str], list[dict]] = defaultdict(list)
    for analysis in analyses:
        buckets[(analysis["map"], analysis["order"])].append(analysis)

    rows = []
    for (map_name, order), bucket in sorted(buckets.items()):
        subject_zoc = [analysis["final_zoc"] for analysis in bucket]
        baseline_zoc = [analysis["baseline_final_zoc"] for analysis in bucket]
        subject_income = [analysis["final_income"] for analysis in bucket]
        baseline_income = [analysis["baseline_final_income"] for analysis in bucket]
        subject_properties = [analysis["final_properties"] for analysis in bucket]
        baseline_properties = [
            analysis["baseline_final_properties"] for analysis in bucket
        ]
        errors = [
            {"seed": analysis["seed"], "error": analysis["error"]}
            for analysis in bucket
            if analysis["error"]
        ]
        missing_metrics = [
            analysis["seed"]
            for analysis in bucket
            if not analysis["metrics_complete"]
        ]
        hard_failures = [
            failure
            for analysis in bucket
            for failure in analysis.get("hard_failures", [])
        ]
        seeds = {analysis["seed"] for analysis in bucket}
        zoc_pass = average(subject_zoc) > average(baseline_zoc)
        income_pass = average(subject_income) >= average(baseline_income)
        properties_pass = average(subject_properties) >= average(baseline_properties)
        trend_pass = all(
            analysis["asset_trend_ok"] and analysis["income_trend_ok"]
            for analysis in bucket
        )
        external_property_pass = (
            all(analysis["external_properties_gained"] > 0 for analysis in bucket)
            if protocol == "v3-selfplay"
            else any(analysis["external_properties_gained"] > 0 for analysis in bucket)
        )
        initial_neutral_pass = all(
            analysis.get("initial_neutral_open", False) for analysis in bucket
        )
        first_expansion_pass = all(
            analysis.get("first_offensive_roi_ranked")
            and (analysis.get("first_roi_offensive") or {}).get("preferred_transport")
            == "TransportHelicopter"
            and int((analysis.get("first_roi_offensive") or {}).get("capture_units", 0))
            >= 2
            for analysis in bucket
        )
        enemy_budget_pass = all(
            analysis.get("enemy_assault_budget_compliant", False)
            for analysis in bucket
        )
        complete = (
            len(bucket) >= 4
            and len(seeds) >= 4
            and not errors
            and not missing_metrics
            and not hard_failures
        )
        if protocol == "v3-selfplay":
            passed = (
                complete
                and external_property_pass
                and initial_neutral_pass
                and first_expansion_pass
                and enemy_budget_pass
            )
        elif map_name == "map_3":
            passed = (
                complete
                and zoc_pass
                and income_pass
                and properties_pass
                and trend_pass
                and external_property_pass
                and initial_neutral_pass
                and first_expansion_pass
                and enemy_budget_pass
            )
        else:
            # map_1/map_2は戦略退行を検知する非悪化行として扱う。
            passed = complete and trend_pass and enemy_budget_pass
        rows.append(
            {
                "map": map_name,
                "order": order,
                "games": len(bucket),
                "unique_seeds": len(seeds),
                "subject_zoc": average(subject_zoc),
                "baseline_zoc": average(baseline_zoc),
                "zoc_pass": zoc_pass,
                "subject_income": average(subject_income),
                "baseline_income": average(baseline_income),
                "income_pass": income_pass,
                "subject_properties": average(subject_properties),
                "baseline_properties": average(baseline_properties),
                "properties_pass": properties_pass,
                "trend_pass": trend_pass,
                "external_property_pass": external_property_pass,
                "initial_neutral_pass": initial_neutral_pass,
                "first_expansion_pass": first_expansion_pass,
                "enemy_budget_pass": enemy_budget_pass,
                "complete": complete,
                "errors": errors,
                "missing_metrics": missing_metrics,
                "hard_failures": hard_failures,
                "passed": passed,
            }
        )

    map3_analyses = [analysis for analysis in analyses if analysis["map"] == "map_3"]
    win_rate = (
        sum(1 for analysis in map3_analyses if analysis["won"])
        / len(map3_analyses)
        if map3_analyses
        else 0.0
    )
    global_hard_failure_codes = list(
        dict.fromkeys(
            code
            for analysis in analyses
            for code in analysis.get("hard_failure_codes", [])
        )
    )
    if protocol == "v3-selfplay":
        if len(results) != 4:
            global_hard_failure_codes.append("selfplay_game_count")
        if len(analyses) != len(results) * 2:
            global_hard_failure_codes.append("selfplay_player_analysis_count")
        overall = (
            len(results) == 4
            and len(analyses) == 8
            and not global_hard_failure_codes
            and all(row["passed"] for row in rows)
        )
    else:
        map3_rows = [row for row in rows if row["map"] == "map_3"]
        overall = (
            {row["order"] for row in map3_rows} == {"先攻", "後攻"}
            and all(row["passed"] for row in rows)
            and win_rate >= 0.40
            and not global_hard_failure_codes
        )

    thinking_times = [
        value
        for analysis in map3_analyses
        for value in analysis.get("thinking_ms", [])
    ]
    summaries = [
        {
            "map": "map_3",
            "games": len(map3_analyses),
            "wins": sum(1 for analysis in map3_analyses if analysis["won"]),
            "win_rate": win_rate,
            "thinking_mean_ms": average(thinking_times),
            "thinking_p95_ms": percentile95(thinking_times),
            "hard_failure_codes": list(dict.fromkeys(global_hard_failure_codes)),
        }
    ]
    if thinking_times:
        summaries[0]["thinking_median_ms"] = statistics.median(thinking_times)
    else:
        summaries[0]["thinking_median_ms"] = 0.0
    return overall, rows, summaries


def write_text_atomic(path: str, content: str) -> None:
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary = target.with_name(f"{target.name}.tmp")
    temporary.write_text(content, encoding="utf-8")
    os.replace(temporary, target)


def write_json_atomic(path: str, payload: dict) -> None:
    write_text_atomic(
        path,
        json.dumps(payload, ensure_ascii=False, indent=2) + "\n",
    )


def _format_optional(value: object, digits: int = 1) -> str:
    if value is None:
        return "-"
    if isinstance(value, float):
        return f"{value:.{digits}f}"
    return str(value)


def generate_issue58_report(payload: dict) -> str:
    metadata = payload.get("metadata", {})
    criteria_rows = payload.get("criteria_rows", [])
    summaries = payload.get("summaries", [])
    analyses = payload.get("analyses", [])
    results = payload.get("results", [])
    lines = ["# Issue #58 Evaluation Report", "", "## Protocol Metadata"]
    lines.extend(
        [
            f"- commit: {metadata.get('commit_sha', '-')}",
            "- working tree: "
            + ("dirty" if metadata.get("working_tree_dirty") else "clean"),
            f"- command: {' '.join(str(part) for part in metadata.get('command', []))}",
            f"- seeds: {', '.join(str(seed) for seed in metadata.get('seeds', []))}",
            f"- games per order: {metadata.get('games_per_order', 0)}",
            f"- protocol: {metadata.get('protocol', '-')}",
            f"- artifact stage: {metadata.get('artifact_stage', '-')}",
            f"- expected games: {metadata.get('expected_games', 0)}",
            f"- games per seed: {metadata.get('games_per_seed', 0)}",
            f"- subject / baseline: {metadata.get('subject', '-')} / {metadata.get('baseline', '-')}",
            f"- evaluator SHA-256: {metadata.get('evaluator_sha256', '-')}",
            f"- analysis evaluator SHA-256: {metadata.get('analysis_evaluator_sha256', metadata.get('evaluator_sha256', '-'))}",
            f"- MCP SHA-256: {metadata.get('mcp_sha256', '-')}",
            "- deterministic repeatability: "
            + ("PASS" if metadata.get("deterministic_repeatability", True) else "FAIL"),
            "",
            "## Schedule Completeness",
            f"- completed / expected games: {len(results)} / {metadata.get('expected_games', 0)}",
            f"- analysis rows: {len(analyses)}",
            "",
            "## Overall Result",
            "**PASS**" if payload.get("overall_pass") else "**FAIL**",
            "",
            "## Per-Map Comparison",
            "| Map | Order | Games | ZOC subject / baseline | Income subject / baseline | Properties subject / baseline | Trend | External property | Complete | Result |",
            "| --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- | --- |",
        ]
    )
    for row in criteria_rows:
        lines.append(
            "| {map} | {order} | {games} | {subject_zoc:.1f} / {baseline_zoc:.1f} | "
            "{subject_income:.1f} / {baseline_income:.1f} | "
            "{subject_properties:.1f} / {baseline_properties:.1f} | {trend} | "
            "{external} | {complete} | {result} |".format(
                map=row.get("map", "-"),
                order=row.get("order", "-"),
                games=row.get("games", 0),
                subject_zoc=row.get("subject_zoc", 0.0),
                baseline_zoc=row.get("baseline_zoc", 0.0),
                subject_income=row.get("subject_income", 0.0),
                baseline_income=row.get("baseline_income", 0.0),
                subject_properties=row.get("subject_properties", 0.0),
                baseline_properties=row.get("baseline_properties", 0.0),
                trend="PASS" if row.get("trend_pass") else "FAIL",
                external="PASS" if row.get("external_property_pass") else "FAIL",
                complete="yes" if row.get("complete") else "no",
                result="PASS" if row.get("passed") else "FAIL",
            )
        )

    lines.extend(["", "## Win Rate and Thinking Time"])
    for summary in summaries:
        lines.extend(
            [
                f"- map: {summary.get('map', '-')}",
                f"- wins / games: {summary.get('wins', 0)} / {summary.get('games', 0)}",
                f"- win rate: {summary.get('win_rate', 0.0) * 100:.1f}%",
                f"- thinking mean / median / p95: {summary.get('thinking_mean_ms', 0.0):.1f} / "
                f"{summary.get('thinking_median_ms', 0.0):.1f} / "
                f"{summary.get('thinking_p95_ms', 0.0):.1f} ms",
            ]
        )

    lines.extend(
        [
            "",
            "## Per-Player Behavior",
            "| Map | Seed | Player | Campaign turns | Max offensives | First OpenNeutral | External capture | Behavior |",
            "| --- | ---: | ---: | ---: | ---: | --- | --- | --- |",
        ]
    )
    for analysis in analyses:
        lines.append(
            f"| {analysis.get('map', '-')} | {analysis.get('seed', '-')} | "
            f"{analysis.get('subject_player', '-')} | {analysis.get('campaign_turns', 0)} | "
            f"{analysis.get('max_simultaneous_offensives', 0)} | "
            f"{'PASS' if analysis.get('first_offensive_open_neutral') else 'FAIL'} | "
            f"{'PASS' if analysis.get('external_properties_gained', 0) > 0 else 'FAIL'} | "
            f"{'PASS' if analysis.get('behavior_pass') else 'FAIL'} |"
        )

    lines.extend(
        [
            "",
            "## Occupation Throughput by Seed and Order",
            "| Map | Seed | Order | Landed capture units | Capture started | Capture completed | External gained | Retained | Lost | Landing to capture |",
            "| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for analysis in analyses:
        lines.append(
            f"| {analysis.get('map', '-')} | {analysis.get('seed', '-')} | {analysis.get('order', '-')} | "
            f"{analysis.get('landed_capture_units', 0)} | {analysis.get('capture_started', 0)} | "
            f"{analysis.get('capture_completed', 0)} | {analysis.get('external_properties_gained', 0)} | "
            f"{analysis.get('external_properties_retained', 0)} | "
            f"{analysis.get('external_properties_lost_after_capture', 0)} | "
            f"{_format_optional(analysis.get('landing_to_capture_turns'))} |"
        )

    total_investment: defaultdict[str, int] = defaultdict(int)
    for analysis in analyses:
        for unit_type, cost in analysis.get("production_investment", {}).items():
            total_investment[unit_type] += int(cost)
    lines.extend(
        [
            "",
            "## Production Investment by Unit Type",
            "| Unit type | Investment |",
            "| --- | ---: |",
        ]
    )
    for unit_type, cost in sorted(total_investment.items()):
        lines.append(f"| {unit_type} | {cost} |")

    lines.extend(
        [
            "",
            "## Battleship Investment and ROI by Game",
            "| Map | Seed | Order | Investment | Damage value | ROI |",
            "| --- | ---: | --- | ---: | ---: | ---: |",
        ]
    )
    for analysis in analyses:
        investment = analysis.get("battleship_investment", 0)
        damage = analysis.get("battleship_damage_value", 0)
        lines.append(
            f"| {analysis.get('map', '-')} | {analysis.get('seed', '-')} | "
            f"{analysis.get('order', '-')} | {investment} | {damage} | "
            f"{_format_optional(analysis.get('battleship_roi'), 4)} |"
        )

    lines.extend(
        [
            "",
            "## Invasion Milestones",
            "| Map | Seed | Order | Transport production | Load | Drop | Combat | Capture start | Capture complete |",
            "| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for analysis in analyses:
        milestone = analysis.get("milestones", {})
        lines.append(
            f"| {analysis.get('map', '-')} | {analysis.get('seed', '-')} | "
            f"{analysis.get('order', '-')} | "
            f"{_format_optional(milestone.get('first_transport_production'))} | "
            f"{_format_optional(milestone.get('first_load'))} | "
            f"{_format_optional(milestone.get('first_drop'))} | "
            f"{_format_optional(milestone.get('first_combat'))} | "
            f"{_format_optional(milestone.get('first_capture_start'))} | "
            f"{_format_optional(milestone.get('first_capture_complete'))} |"
        )

    lines.extend(["", "## Hard Failures"])
    hard_failures = [
        (analysis, failure)
        for analysis in analyses
        for failure in analysis.get("hard_failures", [])
    ]
    if hard_failures:
        for analysis, failure in hard_failures:
            lines.append(
                f"- map={analysis.get('map', '-')} seed={analysis.get('seed', '-')} "
                f"player={analysis.get('subject_player', '-')}: "
                f"{failure.get('code', 'unknown')} {failure.get('message', '')}"
            )
    global_hard_codes = list(
        dict.fromkeys(
            code
            for summary in summaries
            for code in summary.get("hard_failure_codes", [])
        )
    )
    analysis_hard_codes = {
        failure.get("code") for _, failure in hard_failures
    }
    for code in global_hard_codes:
        if code not in analysis_hard_codes:
            lines.append(f"- {code}")
    if not hard_failures and not global_hard_codes:
        lines.append("- none")

    lines.extend(["", "## Baseline Comparison"])
    baseline_rows = payload.get("baseline_comparison", [])
    if baseline_rows:
        for row in baseline_rows:
            lines.append(
                f"- {row.get('map', '-')} {row.get('order', '-')}: "
                f"ratio={_format_optional(row.get('thinking_ratio'), 3)} "
                f"{'PASS' if row.get('thinking_time_pass') else 'FAIL'}"
            )
    else:
        lines.append("- not applicable")

    lines.extend(
        [
            "",
            "## Artifact Paths",
            f"- current: {metadata.get('artifact_path', '-')}",
            f"- baseline: {metadata.get('baseline_artifact_path', '-')}",
            "",
            "## Errors",
        ]
    )
    errors = [
        result
        for result in results
        if result.get("error")
    ]
    if errors:
        for result in errors:
            lines.append(
                f"- {result.get('map', '-')} seed={result.get('seed', '-')} "
                f"P1={result.get('p1', '-')} P2={result.get('p2', '-')}: "
                f"{result.get('error')}"
            )
    else:
        lines.append("- none")
    lines.append("")
    return "\n".join(lines)
