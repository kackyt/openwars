import unittest

from scripts.eval_matchup import analyze_issue54_game, judge_issue54_criteria


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


if __name__ == "__main__":
    unittest.main()
