import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from scripts.eval_issue58 import (
    analyze_issue58_game,
    analyze_issue58_player,
    collect_run_metadata,
    compare_issue58_baseline,
    generate_issue58_report,
    judge_issue58_criteria,
    parse_seed_list,
    sha256_files,
    validate_issue58_run,
    write_json_atomic,
    write_text_atomic,
)


class Issue58SeedProtocolTests(unittest.TestCase):
    def test_parse_seed_list_preserves_explicit_order(self):
        self.assertEqual(
            (58001, 58002, 58003, 58004),
            parse_seed_list("58001,58002,58003,58004"),
        )

    def test_parse_seed_list_rejects_duplicate_seed(self):
        with self.assertRaisesRegex(ValueError, "seed must be unique"):
            parse_seed_list("58001,58002,58001,58004")

    def test_issue58_requires_fixed_seed_set(self):
        with self.assertRaisesRegex(ValueError, "58001,58002,58003,58004"):
            validate_issue58_run(
                protocol="v3-selfplay",
                artifact_stage="baseline",
                maps=("map_3",),
                subject="V3",
                baseline="V3",
                max_turns=30,
                seeds=(1, 2, 3, 4),
                markdown_output="benchmarks/issue-58/baseline.md",
                json_output="benchmarks/issue-58/baseline.json",
            )

    def test_issue58_rejects_matchup_report_output(self):
        with self.assertRaisesRegex(ValueError, "matchup_report.md"):
            validate_issue58_run(
                protocol="v3-selfplay",
                artifact_stage="baseline",
                maps=("map_3",),
                subject="V3",
                baseline="V3",
                max_turns=30,
                seeds=(58001, 58002, 58003, 58004),
                markdown_output="matchup_report.md",
                json_output="benchmarks/issue-58/baseline.json",
            )


class Issue58MetadataTests(unittest.TestCase):
    def test_sha256_files_changes_when_content_changes(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "evaluator.py"
            path.write_text("a", encoding="utf-8")
            first = sha256_files((str(path),))
            path.write_text("b", encoding="utf-8")
            self.assertNotEqual(first, sha256_files((str(path),)))

    @patch("scripts.eval_issue58.subprocess.run")
    def test_collect_run_metadata_records_commit_dirty_state_and_trials(self, run):
        run.side_effect = [
            type("Result", (), {"stdout": "abc123\n"})(),
            type("Result", (), {"stdout": " M scripts/eval_matchup.py\n"})(),
        ]
        with TemporaryDirectory() as directory:
            evaluator = Path(directory) / "eval.py"
            mcp = Path(directory) / "trace.rs"
            evaluator.write_text("eval", encoding="utf-8")
            mcp.write_text("trace", encoding="utf-8")
            metadata = collect_run_metadata(
                ["python", "scripts/eval_matchup.py"],
                (1, 2, 3, 4),
                (str(evaluator),),
                (str(mcp),),
            )
        self.assertEqual("abc123", metadata["commit_sha"])
        self.assertTrue(metadata["working_tree_dirty"])
        self.assertEqual(4, metadata["games_per_order"])


def make_issue58_game():
    return {
        "map": "map_3",
        "p1": "V2",
        "p2": "V3",
        "seed": 58001,
        "result": "P2_Win_MaxTurns",
        "error": None,
        "thinking_times": {1: [10.0], 2: [12.0]},
        "initial_state": {
            "properties": [
                {
                    "x": 0,
                    "y": 0,
                    "terrain": "Capital",
                    "owner": 1,
                    "island_id": 0,
                },
                {
                    "x": 9,
                    "y": 9,
                    "terrain": "Capital",
                    "owner": 2,
                    "island_id": 5,
                },
            ]
        },
        "final_state": {
            "properties": [
                {
                    "x": 0,
                    "y": 0,
                    "terrain": "Capital",
                    "owner": 1,
                    "island_id": 0,
                },
                {
                    "x": 9,
                    "y": 9,
                    "terrain": "Capital",
                    "owner": 2,
                    "island_id": 5,
                },
                {
                    "x": 1,
                    "y": 0,
                    "terrain": "City",
                    "owner": 2,
                    "island_id": 0,
                },
            ],
            "units": [
                {
                    "unit_id": 20,
                    "player_id": 2,
                    "unit_type": "Infantry",
                    "can_capture": True,
                    "hp": 70,
                    "x": 1,
                    "y": 0,
                    "island_id": 0,
                },
            ],
        },
        "metrics": [
            {
                "turn": 30,
                "p1_units": 10_000,
                "p2_units": 12_000,
                "p1_obj": {
                    "zoc_area": 10,
                    "income_per_turn": 4_000,
                    "owned_properties": 1,
                },
                "p2_obj": {
                    "zoc_area": 12,
                    "income_per_turn": 5_000,
                    "owned_properties": 2,
                },
            }
        ],
        "invasion_events": [
            {
                "type": "unit_produced",
                "turn": 1,
                "player_id": 2,
                "unit_id": 30,
                "unit_type": "Lander",
                "cost": 12_000,
                "max_cargo": 2,
                "can_capture": False,
                "x": 9,
                "y": 8,
            },
            {
                "type": "unit_produced",
                "turn": 2,
                "player_id": 2,
                "unit_id": 40,
                "unit_type": "Battleship",
                "cost": 28_000,
                "max_cargo": 0,
                "can_capture": False,
                "x": 9,
                "y": 8,
            },
            {
                "type": "unit_loaded",
                "turn": 3,
                "player_id": 2,
                "transport_id": 10,
                "cargo_id": 20,
                "island_id": 5,
            },
            {
                "type": "unit_unloaded",
                "turn": 6,
                "player_id": 2,
                "transport_id": 10,
                "cargo_id": 20,
                "unit_type": "Infantry",
                "can_capture": True,
                "island_id": 0,
                "x": 1,
                "y": 0,
            },
            {
                "type": "property_capture_progressed",
                "turn": 7,
                "player_id": 2,
                "unit_id": 20,
                "island_id": 0,
                "x": 1,
                "y": 0,
                "completed": False,
            },
            {
                "type": "property_capture_progressed",
                "turn": 8,
                "player_id": 2,
                "unit_id": 20,
                "island_id": 0,
                "x": 1,
                "y": 0,
                "completed": True,
            },
            {
                "type": "unit_attacked",
                "turn": 9,
                "player_id": 2,
                "attacker_player_id": 2,
                "attacker_unit_type": "Battleship",
                "defender_player_id": 1,
                "defender_unit_type": "Infantry",
                "damage_value_dealt": 500,
                "counter_value_received": 0,
            },
        ],
        "strategic_history": [
            {
                "turn": 6,
                "properties": [],
                "units": [
                    {
                        "unit_id": 20,
                        "player_id": 2,
                        "unit_type": "Infantry",
                        "can_capture": True,
                        "hp": 100,
                        "x": 1,
                        "y": 0,
                        "island_id": 0,
                    },
                ],
            },
            {
                "turn": 8,
                "properties": [
                    {
                        "x": 1,
                        "y": 0,
                        "terrain": "City",
                        "owner": 2,
                        "island_id": 0,
                    },
                ],
                "units": [
                    {
                        "unit_id": 20,
                        "player_id": 2,
                        "unit_type": "Infantry",
                        "can_capture": True,
                        "hp": 70,
                        "x": 1,
                        "y": 0,
                        "island_id": 0,
                    },
                ],
            },
        ],
    }


def campaign_assessment(
    island_id,
    state,
    decision,
    required_budget=0,
    allocated_budget=0,
    enemy_arrival_eta=None,
):
    return {
        "island_id": island_id,
        "state": state,
        "decision": decision,
        "state_reason": f"state-{state}",
        "decision_reason": f"decision-{decision}",
        "neutral_properties": 1 if state == "OpenNeutral" else 0,
        "friendly_properties": 1 if state in {"Secured", "Threatened"} else 0,
        "enemy_properties": 1 if state == "EnemyHeld" else 0,
        "friendly_combat_value": 1000,
        "enemy_combat_value": 500 if state == "EnemyHeld" else 0,
        "friendly_arrival_eta": 1,
        "enemy_arrival_eta": enemy_arrival_eta,
        "friendly_capture_eta": 2,
        "enemy_capture_eta": None,
        "expansion_payback_turns": 5 if state == "OpenNeutral" else None,
        "required_budget": required_budget,
        "allocated_budget": allocated_budget,
    }


def campaign_assignment(
    island_id,
    decision="Expand",
    allocated_budget=6000,
    transport_ids=None,
    capture_ids=None,
    combat_ids=None,
):
    return {
        "island_id": island_id,
        "decision": decision,
        "target_x": island_id,
        "target_y": 0,
        "requirement": {
            "preferred_transport": "TransportHelicopter",
            "transport_slots": 2,
            "capture_units": 2,
            "combat_budget": 0,
            "total_budget": allocated_budget,
        },
        "purchase_shortfall": {
            "preferred_transport": None,
            "transport_slots": 0,
            "capture_units": 0,
            "combat_budget": 0,
            "total_budget": 0,
        },
        "allocated_budget": allocated_budget,
        "transport_entity_ids": list(transport_ids or [101]),
        "capture_entity_ids": list(capture_ids or []),
        "combat_entity_ids": list(combat_ids or []),
        "operation_ready": True,
        "continued_from_existing_squad": False,
    }


def make_portfolio_game(protocol="v3-selfplay"):
    p2 = "V3" if protocol == "v3-selfplay" else "V1"
    assessments = [
        campaign_assessment(0, "Secured", "Secure"),
        campaign_assessment(1, "OpenNeutral", "Expand", 6000, 6000),
        campaign_assessment(2, "EnemyHeld", "Observe", 10000, 0),
    ]
    histories = []
    for player_id in ((1, 2) if protocol == "v3-selfplay" else (1,)):
        histories.append(
            {
                "round": 1,
                "turn": 1,
                "player_id": player_id,
                "available_funds": 5000,
                "units": [
                    {
                        "unit_id": 101,
                        "player_id": player_id,
                        "cost": 1000,
                        "can_capture": False,
                        "max_cargo": 2,
                    }
                ],
                "campaign": {
                    "player_id": player_id,
                    "islands": [dict(item) for item in assessments],
                    "active_offensives": [campaign_assignment(1)],
                    "defenses": [],
                },
            }
        )
    return {
        "map": "map_3",
        "p1": "V3",
        "p2": p2,
        "seed": 58001,
        "result": "Draw_MaxTurns",
        "error": None,
        "thinking_times": {1: [10.0], 2: [12.0]},
        "initial_state": {
            "properties": [
                {"owner": 1, "island_id": 0},
                {"owner": None, "island_id": 1},
                {"owner": 2, "island_id": 2},
            ]
        },
        "final_state": {
            "properties": [
                {"owner": 2, "island_id": 0},
                {"owner": 1, "island_id": 1},
                {"owner": 2, "island_id": 2},
            ],
            "units": [],
        },
        "metrics": [
            {
                "turn": 30,
                "p1_units": 10000,
                "p2_units": 10000,
                "p1_obj": {
                    "zoc_area": 12,
                    "income_per_turn": 5000,
                    "owned_properties": 2,
                },
                "p2_obj": {
                    "zoc_area": 10,
                    "income_per_turn": 4000,
                    "owned_properties": 2,
                },
            }
        ],
        "invasion_events": [],
        "strategic_history": [],
        "island_campaign_history": histories,
    }


class Issue58PortfolioAnalysisTests(unittest.TestCase):
    def test_reports_valid_open_neutral_portfolio_behavior(self):
        analysis = analyze_issue58_player(make_portfolio_game("v3-v1"), 1, "v3-v1")
        self.assertEqual([], analysis["hard_failure_codes"])
        self.assertTrue(analysis["initial_neutral_open"])
        self.assertTrue(analysis["first_offensive_roi_ranked"])
        self.assertEqual(1, analysis["max_simultaneous_offensives"])
        self.assertEqual(1, analysis["external_properties_gained"])

    def test_selfplay_analyzes_both_players(self):
        analyses = analyze_issue58_game(make_portfolio_game(), "v3-selfplay")
        self.assertEqual([1, 2], [row["subject_player"] for row in analyses])

    def test_v3_v1_analyzes_only_v3_player(self):
        analyses = analyze_issue58_game(make_portfolio_game("v3-v1"), "v3-v1")
        self.assertEqual([1], [row["subject_player"] for row in analyses])

    def test_missing_island_in_recorded_turn_is_hard_failure(self):
        game = make_portfolio_game("v3-v1")
        game["island_campaign_history"][0]["campaign"]["islands"].pop()
        analysis = analyze_issue58_player(game, 1, "v3-v1")
        self.assertIn("missing_island_state", analysis["hard_failure_codes"])

    def test_more_than_three_offensives_is_hard_failure(self):
        game = make_portfolio_game("v3-v1")
        campaign = game["island_campaign_history"][0]["campaign"]
        campaign["active_offensives"] = [
            campaign_assignment(island_id) for island_id in (1, 2, 3, 4)
        ]
        analysis = analyze_issue58_player(game, 1, "v3-v1")
        self.assertIn("too_many_offensives", analysis["hard_failure_codes"])

    def test_entity_assigned_to_two_islands_is_hard_failure(self):
        game = make_portfolio_game("v3-v1")
        campaign = game["island_campaign_history"][0]["campaign"]
        campaign["active_offensives"] = [
            campaign_assignment(1, transport_ids=[101]),
            campaign_assignment(2, decision="Assault", transport_ids=[101]),
        ]
        analysis = analyze_issue58_player(game, 1, "v3-v1")
        self.assertIn("duplicate_entity_assignment", analysis["hard_failure_codes"])

    def test_budget_above_funds_and_allocated_assets_is_hard_failure(self):
        game = make_portfolio_game("v3-v1")
        assignment = game["island_campaign_history"][0]["campaign"][
            "active_offensives"
        ][0]
        assignment["allocated_budget"] = 7000
        analysis = analyze_issue58_player(game, 1, "v3-v1")
        self.assertIn("allocated_budget_exceeds_resources", analysis["hard_failure_codes"])

    def test_underfunded_enemy_held_assault_is_hard_failure(self):
        game = make_portfolio_game("v3-v1")
        campaign = game["island_campaign_history"][0]["campaign"]
        campaign["islands"][2].update(
            {"decision": "Assault", "required_budget": 10000, "allocated_budget": 5000}
        )
        campaign["active_offensives"].append(
            campaign_assignment(2, decision="Assault", allocated_budget=5000)
        )
        analysis = analyze_issue58_player(game, 1, "v3-v1")
        self.assertIn("underfunded_enemy_assault", analysis["hard_failure_codes"])

    def test_game_error_is_hard_failure(self):
        game = make_portfolio_game("v3-v1")
        game["error"] = "MCP failed"
        analysis = analyze_issue58_player(game, 1, "v3-v1")
        self.assertIn("game_error", analysis["hard_failure_codes"])

    def test_selfplay_missing_second_player_analysis_fails_criteria(self):
        game = make_portfolio_game("v3-selfplay")
        game["p2"] = "V1"
        game["island_campaign_history"] = game["island_campaign_history"][:1]
        overall, _, summaries = judge_issue58_criteria(
            [game], protocol="v3-selfplay"
        )
        self.assertFalse(overall)
        self.assertIn(
            "selfplay_player_analysis_count",
            summaries[0]["hard_failure_codes"],
        )


class Issue58GameAnalysisTests(unittest.TestCase):
    def test_detects_external_property_gain_and_capture_throughput(self):
        from scripts.eval_issue58 import analyze_issue58_game

        analysis = analyze_issue58_player(make_issue58_game(), 2, "v3-v1")
        self.assertEqual("後攻", analysis["order"])
        self.assertEqual(1, analysis["external_properties_gained"])
        self.assertEqual(1, analysis["capture_started"])
        self.assertEqual(1, analysis["capture_completed"])
        self.assertEqual(1, analysis["landing_to_capture_turns"])
        self.assertEqual(1, analysis["external_properties_retained"])
        self.assertEqual(0, analysis["external_properties_lost_after_capture"])
        self.assertEqual(1.0, analysis["capture_unit_survival_rate"])
        self.assertEqual(2, analysis["transport_capacity_produced"])
        self.assertEqual(1, analysis["milestones"]["first_transport_production"])
        self.assertEqual(
            {"turn": 8, "island_id": 0, "x": 1, "y": 0},
            analysis["first_external_property_capture"],
        )

    def test_calculates_battleship_investment_and_damage_roi(self):
        from scripts.eval_issue58 import analyze_issue58_game

        analysis = analyze_issue58_player(make_issue58_game(), 2, "v3-v1")
        self.assertEqual(28_000, analysis["production_investment"]["Battleship"])
        self.assertEqual(500, analysis["combat_value_by_unit_type"]["Battleship"])
        self.assertAlmostEqual(500 / 28_000, analysis["battleship_roi"])

    def test_calculates_localized_battleship_damage_roi(self):
        from scripts.eval_issue58 import analyze_issue58_game

        game = make_issue58_game()
        game["invasion_events"][1]["unit_type"] = "戦艦"
        game["invasion_events"][-1]["attacker_unit_type"] = "戦艦"
        analysis = analyze_issue58_player(game, 2, "v3-v1")
        self.assertEqual(28_000, analysis["production_investment"]["戦艦"])
        self.assertEqual(500, analysis["combat_value_by_unit_type"]["戦艦"])
        self.assertAlmostEqual(500 / 28_000, analysis["battleship_roi"])

    def test_ignores_capture_progress_on_the_subject_initial_island(self):
        from scripts.eval_issue58 import analyze_issue58_game

        game = make_issue58_game()
        game["invasion_events"].insert(
            4,
            {
                "type": "property_capture_progressed",
                "turn": 2,
                "player_id": 2,
                "unit_id": 99,
                "island_id": 5,
                "x": 8,
                "y": 9,
                "completed": True,
            },
        )
        analysis = analyze_issue58_player(game, 2, "v3-v1")
        self.assertEqual(1, analysis["capture_started"])
        self.assertEqual(1, analysis["capture_completed"])
        self.assertEqual(7, analysis["milestones"]["first_capture_start"])

    def test_apc_cargo_capacity_is_not_counted_as_invasion_transport(self):
        from scripts.eval_issue58 import analyze_issue58_game

        game = make_issue58_game()
        game["invasion_events"].insert(
            0,
            {
                "type": "unit_produced",
                "turn": 0,
                "player_id": 2,
                "unit_id": 29,
                "unit_type": "装甲車",
                "cost": 4_200,
                "max_cargo": 1,
                "can_capture": False,
                "x": 9,
                "y": 9,
            },
        )
        analysis = analyze_issue58_player(game, 2, "v3-v1")
        self.assertEqual(2, analysis["transport_capacity_produced"])
        self.assertEqual(1, analysis["milestones"]["first_transport_production"])


def metric_game(
    order,
    seed,
    subject_zoc,
    baseline_zoc,
    subject_income,
    baseline_income,
):
    p1, p2 = ("V3", "V2") if order == "先攻" else ("V2", "V3")
    subject_side = "p1" if p1 == "V3" else "p2"
    baseline_side = "p2" if subject_side == "p1" else "p1"
    subject_player = 1 if subject_side == "p1" else 2
    metric = {
        "turn": 30,
        "p1_units": 10_000,
        "p2_units": 10_000,
        "p1_obj": {},
        "p2_obj": {},
    }
    metric[f"{subject_side}_obj"] = {
        "zoc_area": subject_zoc,
        "income_per_turn": subject_income,
        "owned_properties": 2,
    }
    metric[f"{baseline_side}_obj"] = {
        "zoc_area": baseline_zoc,
        "income_per_turn": baseline_income,
        "owned_properties": 1,
    }
    return {
        "map": "map_3",
        "p1": p1,
        "p2": p2,
        "seed": seed,
        "result": "P1_Win_MaxTurns",
        "error": None,
        "initial_state": {
            "properties": [
                {"owner": 1, "island_id": 0},
                {"owner": 2, "island_id": 5},
            ]
        },
        "final_state": {
            "properties": [
                {"owner": 1, "island_id": 0},
                {"owner": 2, "island_id": 5},
                {
                    "owner": subject_player,
                    "island_id": 5 if subject_player == 1 else 0,
                },
            ]
        },
        "metrics": [dict(metric, turn=turn) for turn in range(1, 16)],
        "invasion_events": [],
        "strategic_history": [],
        "thinking_times": {1: [10.0], 2: [10.0]},
    }


class Issue58CriteriaTests(unittest.TestCase):
    def test_equal_income_passes_but_equal_zoc_fails(self):
        games = []
        for seed in (1, 2, 3, 4):
            games.append(metric_game("先攻", seed, 10, 10, 5000, 5000))
            games.append(metric_game("後攻", seed, 10, 10, 5000, 5000))
        overall, rows, _ = judge_issue58_criteria(games)
        self.assertFalse(overall)
        self.assertTrue(all(row["income_pass"] for row in rows))
        self.assertTrue(all(not row["zoc_pass"] for row in rows))

    def test_missing_seed_or_error_fails_the_order_bucket(self):
        games = [
            metric_game("先攻", seed, 11, 10, 5000, 5000)
            for seed in (1, 2, 3, 4)
        ]
        games += [
            metric_game("後攻", seed, 11, 10, 5000, 5000)
            for seed in (1, 2, 3)
        ]
        games[0]["error"] = "MCP failed"
        overall, rows, _ = judge_issue58_criteria(games)
        self.assertFalse(overall)
        self.assertTrue(any(row["errors"] for row in rows))
        self.assertTrue(any(row["games"] < 4 for row in rows))

    def test_missing_metrics_fails_the_order_bucket(self):
        games = []
        for seed in (1, 2, 3, 4):
            games.append(metric_game("先攻", seed, 11, 10, 5000, 5000))
            games.append(metric_game("後攻", seed, 11, 10, 5000, 5000))
        games[0]["metrics"] = []
        overall, rows, _ = judge_issue58_criteria(games)
        self.assertFalse(overall)
        self.assertTrue(any(row["missing_metrics"] for row in rows))


class Issue58BaselineComparisonTests(unittest.TestCase):
    def payload(self, stage, commit_sha, artifact_path, thinking_ms):
        return {
            "metadata": {
                "protocol": "v3-v1",
                "artifact_stage": stage,
                "seeds": [58001, 58002, 58003, 58004],
                "expected_games": 24,
                "commit_sha": commit_sha,
                "artifact_path": artifact_path,
                "evaluator_sha256": "e" * 64,
                "mcp_sha256": "m" * 64,
            },
            "analyses": [
                {
                    "map": "map_3",
                    "order": "先攻",
                    "thinking_ms": thinking_ms,
                }
            ],
        }

    def test_result_stage_requires_baseline_json(self):
        with self.assertRaisesRegex(ValueError, "baseline-json"):
            validate_issue58_run(
                "v3-v1",
                "result",
                ("map_1", "map_2", "map_3"),
                "V3",
                "V1",
                30,
                (58001, 58002, 58003, 58004),
                "result.md",
                "result.json",
                baseline_json=None,
            )

    def test_compares_thinking_time_for_same_map_and_order(self):
        baseline = self.payload("baseline", "base", "baseline.json", [100.0, 120.0])
        result = self.payload("result", "result", "result.json", [130.0, 140.0])
        rows = compare_issue58_baseline(result, baseline)
        self.assertEqual(1, len(rows))
        self.assertEqual("map_3", rows[0]["map"])
        self.assertEqual("先攻", rows[0]["order"])
        self.assertLessEqual(rows[0]["thinking_ratio"], 1.5)
        self.assertTrue(rows[0]["thinking_time_pass"])

    def test_rejects_mismatched_protocol_before_comparison(self):
        baseline = self.payload("baseline", "base", "baseline.json", [100.0])
        result = self.payload("result", "result", "result.json", [100.0])
        baseline["metadata"]["protocol"] = "v3-selfplay"
        with self.assertRaisesRegex(ValueError, "protocol"):
            compare_issue58_baseline(result, baseline)


class Issue58ArtifactTests(unittest.TestCase):
    def test_report_contains_reproducibility_metadata(self):
        report = generate_issue58_report(
            {
                "metadata": {
                    "commit_sha": "abc123",
                    "working_tree_dirty": True,
                    "command": ["python", "scripts/eval_matchup.py"],
                    "seeds": [1, 2, 3, 4],
                    "games_per_order": 4,
                    "protocol": "v3-v1",
                    "artifact_stage": "baseline",
                    "expected_games": 24,
                    "games_per_seed": 6,
                    "subject": "V3",
                    "baseline": "V1",
                    "evaluator_sha256": "e" * 64,
                    "analysis_evaluator_sha256": "a" * 64,
                    "mcp_sha256": "m" * 64,
                    "deterministic_repeatability": False,
                },
                "overall_pass": False,
                "criteria_rows": [],
                "summaries": [],
                "analyses": [],
                "results": [],
            }
        )
        self.assertIn("abc123", report)
        self.assertIn("working tree: dirty", report)
        self.assertIn("1, 2, 3, 4", report)
        self.assertIn("games per order: 4", report)
        self.assertIn("protocol: v3-v1", report)
        self.assertIn("artifact stage: baseline", report)
        self.assertIn("expected games: 24", report)
        self.assertIn("games per seed: 6", report)
        self.assertIn("subject / baseline: V3 / V1", report)
        self.assertIn("analysis evaluator SHA-256", report)
        self.assertIn("deterministic repeatability: FAIL", report)
        for heading in (
            "## Protocol Metadata",
            "## Schedule Completeness",
            "## Per-Map Comparison",
            "## Per-Player Behavior",
            "## Hard Failures",
            "## Baseline Comparison",
            "## Artifact Paths",
        ):
            self.assertIn(heading, report)

    def test_atomic_writers_create_parent_and_final_file(self):
        with TemporaryDirectory() as directory:
            text_path = Path(directory) / "nested" / "report.md"
            json_path = Path(directory) / "nested" / "report.json"
            write_text_atomic(str(text_path), "report")
            write_json_atomic(str(json_path), {"ok": True})
            self.assertEqual("report", text_path.read_text(encoding="utf-8"))
            self.assertEqual(
                {"ok": True},
                json.loads(json_path.read_text(encoding="utf-8")),
            )
            self.assertFalse(text_path.with_suffix(".md.tmp").exists())


if __name__ == "__main__":
    unittest.main()
