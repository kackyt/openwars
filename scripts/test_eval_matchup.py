import json
import subprocess
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from scripts import eval_issue58, eval_matchup
from scripts.eval_matchup import (
    analyze_issue54_game,
    build_match_specs,
    judge_issue54_criteria,
    run_single_game,
)


def make_game(p1="V3", p2="V2", events=None, history=None):
    return {
        "map": "map_3",
        "p1": p1,
        "p2": p2,
        "initial_state": {
            "properties": [
                {"terrain": "Capital", "owner": 1, "island_id": 0},
                {"terrain": "Capital", "owner": 2, "island_id": 5},
            ]
        },
        "invasion_events": events or [],
        "transport_history": history or [],
    }


def successful_events(player_id, enemy_island):
    return [
        {
            "type": "unit_loaded",
            "turn": 2,
            "player_id": player_id,
            "step": 0,
            "transport_id": 10,
            "cargo_id": 20,
            "island_id": 0 if player_id == 1 else 5,
        },
        {
            "type": "unit_unloaded",
            "turn": 5,
            "player_id": player_id,
            "step": 0,
            "transport_id": 10,
            "cargo_id": 20,
            "x": 24 if player_id == 1 else 5,
            "y": 24 if player_id == 1 else 5,
            "island_id": enemy_island,
        },
        {
            "type": "unit_attacked",
            "turn": 5,
            "player_id": 2 if player_id == 1 else 1,
            "step": 1,
            "attacker_id": 99,
            "defender_id": 20,
        },
    ]


class Issue54EvaluatorTests(unittest.TestCase):
    def test_correlates_same_cargo_from_load_to_enemy_island_combat(self):
        analysis = analyze_issue54_game(make_game(events=successful_events(1, 5)))
        self.assertTrue(analysis["landing"])
        self.assertTrue(analysis["invasion"])
        self.assertEqual([], analysis["safety_violations"])

    def test_does_not_use_unrelated_attack_as_invasion(self):
        events = successful_events(1, 5)
        events[-1]["defender_id"] = 21
        analysis = analyze_issue54_game(make_game(events=events))
        self.assertTrue(analysis["landing"])
        self.assertFalse(analysis["invasion"])

    def test_rejects_unload_to_non_enemy_capital_island(self):
        analysis = analyze_issue54_game(make_game(events=successful_events(1, 3)))
        self.assertFalse(analysis["landing"])
        self.assertFalse(analysis["invasion"])

    def test_reports_return_with_remaining_cargo(self):
        history = [{
            "turn": 6,
            "player_id": 1,
            "squads": [{
                "player_id": 1,
                "phase": "Return",
                "transport_id": 10,
                "x": 12,
                "y": 12,
                "planned_cargo_ids": [20],
                "loaded_cargo_ids": [],
            }],
        }]
        analysis = analyze_issue54_game(
            make_game(events=successful_events(1, 5), history=history)
        )
        self.assertEqual(1, len(analysis["safety_violations"]))

    def test_reports_unchanged_transit_as_stall(self):
        history = [
            {
                "turn": turn,
                "player_id": 1,
                "squads": [{
                    "player_id": 1,
                    "phase": "Transit",
                    "transport_id": 10,
                    "x": 12,
                    "y": 12,
                    "planned_cargo_ids": [20],
                    "loaded_cargo_ids": [20],
                }],
            }
            for turn in range(1, 4)
        ]
        analysis = analyze_issue54_game(
            make_game(events=successful_events(1, 5), history=history),
            stall_turns=3,
        )
        self.assertEqual(1, len(analysis["safety_violations"]))

    def test_requires_pass_for_both_orders(self):
        first = make_game(events=successful_events(1, 5))
        second = make_game(
            p1="V2",
            p2="V3",
            events=successful_events(2, 0),
        )
        overall, rows, _ = judge_issue54_criteria([first, second])
        self.assertTrue(overall)
        self.assertEqual({"先攻", "後攻"}, {row["order"] for row in rows})


class MatchSchedulingTests(unittest.TestCase):
    def test_build_match_specs_runs_each_seed_in_both_orders(self):
        specs = build_match_specs(("map_3",), "V3", "V2", (11, 22))
        self.assertEqual(
            [
                {"map": "map_3", "p1": "V3", "p2": "V2", "seed": 11, "grid_type": "hex"},
                {"map": "map_3", "p1": "V2", "p2": "V3", "seed": 11, "grid_type": "hex"},
                {"map": "map_3", "p1": "V3", "p2": "V2", "seed": 22, "grid_type": "hex"},
                {"map": "map_3", "p1": "V2", "p2": "V3", "seed": 22, "grid_type": "hex"},
            ],
            specs,
        )


class Issue58PortfolioSchedulingTests(unittest.TestCase):
    def test_v3_v1_protocol_builds_24_games_in_both_orders(self):
        specs = eval_matchup.build_issue58_match_specs(
            "v3-v1",
            ("map_1", "map_2", "map_3"),
            (58001, 58002, 58003, 58004),
        )

        self.assertEqual(len(specs), 24)
        self.assertEqual(
            specs[:2],
            [
                {"map": "map_1", "p1": "V3", "p2": "V1", "seed": 58001, "grid_type": "hex"},
                {"map": "map_1", "p1": "V1", "p2": "V3", "seed": 58001, "grid_type": "hex"},
            ],
        )

    def test_v3_selfplay_protocol_builds_one_game_per_seed(self):
        specs = eval_matchup.build_issue58_match_specs(
            "v3-selfplay",
            ("map_3",),
            (58001, 58002, 58003, 58004),
        )

        self.assertEqual(len(specs), 4)
        self.assertTrue(all(spec["p1"] == spec["p2"] == "V3" for spec in specs))
        self.assertEqual(
            [spec["seed"] for spec in specs],
            [58001, 58002, 58003, 58004],
        )

    def test_selfplay_rejects_maps_other_than_map_3(self):
        with self.assertRaisesRegex(ValueError, "map_3 only"):
            eval_issue58.validate_issue58_run(
                "v3-selfplay",
                "baseline",
                ("map_1", "map_3"),
                "V3",
                "V3",
                30,
                (58001, 58002, 58003, 58004),
                "selfplay.md",
                "selfplay.json",
            )


class Issue58CliTests(unittest.TestCase):
    def run_issue58_main(
        self,
        directory,
        protocol="v3-v1",
        artifact_stage="baseline",
        runtime_error=None,
        criteria_pass=False,
        criteria_complete=False,
    ):
        maps = "map_1,map_2,map_3" if protocol == "v3-v1" else "map_3"
        baseline = "V1" if protocol == "v3-v1" else "V3"
        markdown_path = Path(directory) / f"{protocol}-{artifact_stage}.md"
        json_path = Path(directory) / f"{protocol}-{artifact_stage}.json"
        baseline_path = Path(directory) / f"{protocol}-baseline-source.json"
        if artifact_stage == "result":
            baseline_path.write_text(
                json.dumps(
                    {
                        "metadata": {
                            "protocol": protocol,
                            "artifact_stage": "baseline",
                            "seeds": [58001, 58002, 58003, 58004],
                            "expected_games": 24 if protocol == "v3-v1" else 4,
                            "commit_sha": "baseline-sha",
                            "artifact_path": str(baseline_path.resolve()),
                            "evaluator_sha256": "e" * 64,
                            "mcp_sha256": "m" * 64,
                        },
                        "analyses": [
                            {
                                "map": "map_3",
                                "order": "先攻",
                                "thinking_ms": [10.0],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
        argv = [
            "scripts/eval_matchup.py",
            "--mode",
            "batch",
            "--map",
            maps,
            "--p1",
            "V3",
            "--p2",
            baseline,
            "--criteria",
            "issue58",
            "--issue58-protocol",
            protocol,
            "--artifact-stage",
            artifact_stage,
            "--seeds",
            "58001,58002,58003,58004",
            "--max-turns",
            "30",
            "--json-output",
            str(json_path),
            "--output",
            str(markdown_path),
        ]
        if artifact_stage == "result":
            argv.extend(["--baseline-json", str(baseline_path)])
        fake_result = {
            "result": "Draw_MaxTurns",
            "turns": 30,
            "error": runtime_error,
        }
        criteria_rows = [{"complete": criteria_complete}]
        eval_matchup.p = None
        with (
            patch.object(sys, "argv", argv),
            patch("builtins.print"),
            patch.object(eval_matchup, "init_mcp_server"),
            patch.object(eval_matchup, "run_single_game", return_value=fake_result.copy()),
            patch.object(
                eval_matchup,
                "analyze_issue58_game",
                return_value=[
                    {
                        "map": "map_3",
                        "order": "先攻",
                        "thinking_ms": [10.0],
                    }
                ],
            ),
            patch.object(
                eval_matchup,
                "judge_issue58_criteria",
                return_value=(criteria_pass, criteria_rows, []),
            ),
            patch.object(eval_matchup, "generate_issue58_report", return_value="report"),
            patch.object(
                eval_matchup,
                "collect_run_metadata",
                return_value={
                    "commit_sha": "abc123",
                    "working_tree_dirty": True,
                    "command": argv,
                    "seeds": [58001, 58002, 58003, 58004],
                    "games_per_order": 4,
                    "evaluator_sha256": "e" * 64,
                    "mcp_sha256": "m" * 64,
                },
            ),
        ):
            eval_matchup.main()
        return markdown_path, json_path

    def test_baseline_writes_v3_v1_artifacts_when_future_criteria_fail(self):
        with TemporaryDirectory() as directory:
            markdown_path, json_path = self.run_issue58_main(directory)
            payload = json.loads(json_path.read_text(encoding="utf-8"))

            self.assertTrue(markdown_path.exists())
            self.assertEqual(24, len(payload["results"]))
            self.assertEqual(
                {
                    "protocol": "v3-v1",
                    "artifact_stage": "baseline",
                    "expected_games": 24,
                    "games_per_seed": 6,
                    "subject": "V3",
                    "baseline": "V1",
                },
                {
                    key: payload["metadata"][key]
                    for key in (
                        "protocol",
                        "artifact_stage",
                        "expected_games",
                        "games_per_seed",
                        "subject",
                        "baseline",
                    )
                },
            )

    def test_selfplay_baseline_writes_one_game_per_seed(self):
        with TemporaryDirectory() as directory:
            _, json_path = self.run_issue58_main(
                directory,
                protocol="v3-selfplay",
            )
            payload = json.loads(json_path.read_text(encoding="utf-8"))

            self.assertEqual(4, payload["metadata"]["expected_games"])
            self.assertEqual(1, payload["metadata"]["games_per_seed"])
            self.assertEqual(4, len(payload["results"]))

    def test_baseline_runtime_error_writes_artifacts_then_exits_two(self):
        with TemporaryDirectory() as directory:
            with self.assertRaisesRegex(SystemExit, "2"):
                self.run_issue58_main(directory, runtime_error="MCP failed")

            self.assertTrue(Path(directory, "v3-v1-baseline.md").exists())
            self.assertTrue(Path(directory, "v3-v1-baseline.json").exists())

    def test_result_acceptance_failure_exits_one_when_execution_is_complete(self):
        with TemporaryDirectory() as directory:
            with self.assertRaisesRegex(SystemExit, "1"):
                self.run_issue58_main(
                    directory,
                    artifact_stage="result",
                    criteria_complete=True,
                )

    def test_result_incomplete_criteria_exits_two(self):
        with TemporaryDirectory() as directory:
            with self.assertRaisesRegex(SystemExit, "2"):
                self.run_issue58_main(directory, artifact_stage="result")


class DirectExecutionImportTests(unittest.TestCase):
    def test_direct_script_uses_sibling_issue58_evaluator(self):
        repository_root = Path(__file__).resolve().parent.parent
        completed = subprocess.run(
            [
                sys.executable,
                str(repository_root / "scripts" / "eval_matchup.py"),
                "--mode",
                "batch",
                "--map",
                "map_3",
                "--p1",
                "V3",
                "--p2",
                "V3",
                "--criteria",
                "issue58",
                "--issue58-protocol",
                "v3-selfplay",
                "--artifact-stage",
                "baseline",
                "--seeds",
                "58001,58002,58003,58004",
                "--max-turns",
                "29",
                "--json-output",
                "invalid.json",
                "--output",
                "invalid.md",
            ],
            cwd=repository_root,
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(2, completed.returncode)
        self.assertIn("Issue #58 requires max_turns=30", completed.stderr)
        self.assertNotIn("TypeError", completed.stderr)


class IslandCampaignCollectionTests(unittest.TestCase):
    def test_run_single_game_collects_player_tagged_campaign_snapshots(self):
        active_index = 0
        campaigns = {
            1: {"player_id": 1, "islands": [{"island_id": 1}]},
            2: {"player_id": 2, "islands": [{"island_id": 2}]},
        }
        deployment_audits = {
            1: {"player_id": 1, "assigned_count": 1, "records": []},
            2: {"player_id": 2, "assigned_count": 2, "records": []},
        }
        emergency_plans = {
            1: {"player_id": 1, "missions": [{"eta": 1}]},
            2: {"player_id": 2, "missions": []},
        }

        def tool(name, arguments=None, req_id=1):
            nonlocal active_index
            if name in {"load_map", "set_player_ai_version"}:
                return {}
            if name == "get_board_state":
                return {
                    "turn": 1,
                    "active_player_index": active_index,
                    "players": [
                        {
                            "player_id": 1,
                            "property_count": 1,
                            "unit_cost": 1000,
                            "funds": 6000,
                        },
                        {
                            "player_id": 2,
                            "property_count": 1,
                            "unit_cost": 1000,
                            "funds": 7000,
                        },
                    ],
                    "properties": [],
                    "units": [],
                    "game_over": None,
                }
            if name == "evaluate_board":
                return {
                    "score": 0,
                    "subjective_metrics": {},
                    "objective_metrics": {},
                }
            if name == "simulate_ai_turn":
                player_id = active_index + 1
                response = {
                    "player_id": player_id,
                    "actions_taken": [],
                    "invasion_events": [],
                    "transport_squads": [],
                    "island_campaign": campaigns[player_id],
                    "deployment_audit": deployment_audits[player_id],
                    "emergency_plan": emergency_plans[player_id],
                }
                active_index = 1 - active_index
                return response
            raise AssertionError(name)

        result = run_single_game(
            "map_3", "V3", "V3", 1, seed=58001, tool_caller=tool
        )

        self.assertEqual(
            [
                {
                    "round": 1,
                    "turn": 1,
                    "player_id": 1,
                    "available_funds": 6000,
                    "units": [],
                    "campaign": campaigns[1],
                },
                {
                    "round": 1,
                    "turn": 1,
                    "player_id": 2,
                    "available_funds": 7000,
                    "units": [],
                    "campaign": campaigns[2],
                },
            ],
            result["island_campaign_history"],
        )
        self.assertEqual(
            [
                {
                    "round": 1,
                    "turn": 1,
                    "player_id": 1,
                    "audit": deployment_audits[1],
                },
                {
                    "round": 1,
                    "turn": 1,
                    "player_id": 2,
                    "audit": deployment_audits[2],
                },
            ],
            result["deployment_audit_history"],
        )
        self.assertEqual(
            [emergency_plans[1], emergency_plans[2]],
            [entry["plan"] for entry in result["emergency_plan_history"]],
        )

        with TemporaryDirectory() as directory:
            trace_path = Path(directory) / "trace.jsonl"
            result.update({"map": "map_3", "p1": "V4", "p2": "V3"})
            eval_matchup.write_trace_jsonl(trace_path, [result])
            lines = [
                json.loads(line)
                for line in trace_path.read_text(encoding="utf-8").splitlines()
            ]
        self.assertEqual(deployment_audits[1], lines[0]["deployment_audit"])
        self.assertEqual(deployment_audits[2], lines[1]["deployment_audit"])
        self.assertEqual(emergency_plans[1], lines[0]["emergency_plan"])
        self.assertEqual(emergency_plans[2], lines[1]["emergency_plan"])


class CompletedRoundTests(unittest.TestCase):
    def test_t30_snapshot_is_taken_after_both_players_finish(self):
        active_index = 0
        completed_actions = 0

        def tool(name, arguments=None, req_id=1):
            nonlocal active_index, completed_actions
            if name in {"load_map", "set_player_ai_version"}:
                return {}
            if name == "get_board_state":
                return {
                    "active_player_index": active_index,
                    "players": [
                        {
                            "player_id": 1,
                            "property_count": 1,
                            "unit_cost": 1000,
                            "funds": 0,
                        },
                        {
                            "player_id": 2,
                            "property_count": 1,
                            "unit_cost": 1000,
                            "funds": 0,
                        },
                    ],
                    "properties": [],
                    "units": [],
                    "game_over": None,
                }
            if name == "evaluate_board":
                return {
                    "score": 0,
                    "subjective_metrics": {},
                    "objective_metrics": {
                        "zoc_area": completed_actions,
                        "income_per_turn": 1000,
                        "owned_properties": 1,
                    },
                }
            if name == "simulate_ai_turn":
                completed_actions += 1
                active_index = 1 - active_index
                return {
                    "actions_taken": [],
                    "invasion_events": [],
                    "transport_squads": [],
                }
            raise AssertionError(name)

        result = run_single_game(
            "map_3", "V3", "V2", 30, seed=7, tool_caller=tool
        )
        self.assertEqual(30, len(result["metrics"]))
        self.assertEqual(60, result["metrics"][-1]["p1_obj"]["zoc_area"])
        self.assertEqual(60, completed_actions)


class FactoryReliefTraceTests(unittest.TestCase):
    def test_write_trace_jsonl_persists_factory_relief_missions(self):
        mission = {
            "assigned_entity": 10,
            "threat_entity": 20,
            "site_x": 5,
            "site_y": 3,
            "site_terrain": "工場",
            "response": "eliminate",
        }
        results = [
            {
                "map": "map_1",
                "p1": "V4",
                "p2": "V3",
                "seed": 42,
                "factory_relief_history": [
                    {
                        "round": 9,
                        "turn": 9,
                        "player_id": 1,
                        "missions": [mission],
                    }
                ],
            }
        ]

        with TemporaryDirectory() as temp_dir:
            output = Path(temp_dir) / "trace.jsonl"
            eval_matchup.write_trace_jsonl(output, results)
            records = [json.loads(line) for line in output.read_text(encoding="utf-8").splitlines()]

        self.assertEqual([mission], records[0]["factory_relief"])
        self.assertEqual(1, records[0]["player_id"])


class GridTypeOptionTests(unittest.TestCase):
    def test_build_match_specs_defaults_to_hex(self):
        specs = build_match_specs(["map_1"], "V3", "V2", [42])
        self.assertEqual("hex", specs[0]["grid_type"])
        self.assertEqual("hex", specs[1]["grid_type"])

    def test_build_match_specs_accepts_square(self):
        specs = build_match_specs(["map_1"], "V3", "V2", [42], grid_type="square")
        self.assertEqual("square", specs[0]["grid_type"])
        self.assertEqual("square", specs[1]["grid_type"])

    def test_run_single_game_passes_grid_type_to_load_map(self):
        load_map_calls = []

        def tool(name, arguments=None, req_id=1):
            if name == "load_map":
                load_map_calls.append(arguments)
                return {}
            if name == "set_player_ai_version":
                return {}
            if name == "get_board_state":
                return {
                    "turn": 1,
                    "active_player_index": 0,
                    "players": [
                        {"player_id": 1, "property_count": 1, "unit_cost": 1000, "funds": 1000},
                        {"player_id": 2, "property_count": 1, "unit_cost": 1000, "funds": 1000},
                    ],
                    "properties": [],
                    "units": [],
                    "game_over": {"status": "winner", "winner_id": 1},
                }
            raise AssertionError(name)

        run_single_game("map_1", "V3", "V2", 1, seed=42, grid_type="square", tool_caller=tool)
        self.assertEqual(1, len(load_map_calls))
        self.assertEqual({"map_name": "map_1", "grid_type": "square", "seed": 42}, load_map_calls[0])


if __name__ == "__main__":
    unittest.main()

