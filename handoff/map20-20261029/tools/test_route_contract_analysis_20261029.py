"""経路切替の集計が欠測・重複を見逃さないことを検証する。"""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("route-contract-analysis-20261029.py")
spec = importlib.util.spec_from_file_location("route_contract_analysis", SCRIPT)
analysis = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analysis)


class RouteContractAnalysisTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "seed.json"
        self.game = {
            "map": "map_20", "p1": "V200", "p2": "V4", "grid_type": "hex",
            "turns": 2, "error": None,
            "capital_route_history": [
                self.route_row(1, [(100, 0), (200, 1), (300, None)]),
                self.route_row(2, [(100, 1), (200, 1), (400, 0)]),
            ],
            "strategic_history": [
                self.board_row(1, 2, [(100, 1, 1), (200, 3, 2)]),
                self.board_row(2, 1, [(100, 2, 1), (200, 3, 2)]),
            ],
        }

    @staticmethod
    def route_row(turn, units):
        return {"player_id": 2, "round": turn, "turn": turn, "routes": {"routes": [
            {"ground_units": [{"entity_id": entity, "route_index": route,
                               "order_resolved": True, "phase": "Advance", "progress": turn}
                              for entity, route in units]}
        ]}}

    @staticmethod
    def board_row(turn, owner, units):
        return {"turn": turn, "properties": [
            {"x": x, "y": y, "owner": owner if (x, y) == (6, 17) else 2}
            for x, y in analysis.WEST
        ], "units": [{"unit_id": entity, "player_id": 2, "x": x, "y": y}
                     for entity, x, y in units]}

    def write_game(self):
        self.path.write_text(json.dumps({"results": [self.game]}), encoding="utf-8")

    def test_counts_only_continuous_entities_and_observed_movement(self):
        self.write_game()
        report = analysis.run(str(self.path), 2)
        self.assertEqual(report["route_switch"]["valid_pairs"], 2)
        self.assertEqual(report["route_switch"]["switches"], 1)
        self.assertEqual(report["route_switch"]["switch_on_west_owner_change_same_step"], 1)
        self.assertEqual(report["route_switch"]["switch_no_property_owner_change"], 0)
        self.assertEqual(report["route_coverage_in_horizon"], {"committed": 5, "uncommitted": 1})
        self.assertEqual(report["resolved_committed_orders"], {"resolved": 5, "unresolved": 0, "unknown": 0})
        self.assertEqual(report["same_route_advance_progress"], {
            "valid_pairs": 1, "advanced": 1, "unchanged": 0, "retreated": 0,
        })
        self.assertEqual(report["moved"], {"den": 2, "moved": 1})

    def test_switch_without_any_property_change_is_counted(self):
        self.game["strategic_history"][1]["properties"][0]["owner"] = 2
        self.write_game()
        report = analysis.run(str(self.path), 2)
        self.assertEqual(report["route_switch"]["switch_no_property_owner_change"], 1)

    def test_unresolved_order_is_not_counted_as_executed_route_progress(self):
        self.game["capital_route_history"][1]["routes"]["routes"][0]["ground_units"][1]["order_resolved"] = False
        self.write_game()
        report = analysis.run(str(self.path), 2)
        self.assertEqual(report["resolved_committed_orders"]["unresolved"], 1)
        self.assertEqual(report["same_route_advance_progress"]["valid_pairs"], 0)

    def test_duplicate_entity_is_rejected(self):
        self.game["capital_route_history"][0]["routes"]["routes"][0]["ground_units"].append(
            {"entity_id": 100, "route_index": 1}
        )
        self.write_game()
        with self.assertRaises(ValueError):
            analysis.run(str(self.path), 2)

    def test_wrong_side_or_game_error_is_rejected(self):
        self.game["p2"] = "V200"
        self.write_game()
        with self.assertRaises(ValueError):
            analysis.run(str(self.path), 2)
        self.game["p2"] = "V4"
        self.game["error"] = "failure"
        self.write_game()
        with self.assertRaises(ValueError):
            analysis.run(str(self.path), 2)

    def test_horizon_excludes_later_transitions(self):
        self.game["turns"] = 3
        self.game["capital_route_history"].append(self.route_row(3, [(100, 0), (200, 0)]))
        self.game["strategic_history"].append(self.board_row(3, 1, [(100, 3, 1), (200, 4, 2)]))
        self.write_game()
        self.assertEqual(analysis.run(str(self.path), 2)["route_switch"]["switches"], 1)
        self.assertEqual(analysis.run(str(self.path), 3)["route_switch"]["switches"], 3)


if __name__ == "__main__":
    unittest.main()
