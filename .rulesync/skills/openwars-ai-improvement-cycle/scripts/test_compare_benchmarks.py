"""誤った比較を成功として扱わないための、集計とCLIの回帰テスト。"""

import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import compare_benchmarks as comparison


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.base = self.root / "base"
        self.new = self.root / "new"
        self.base.mkdir()
        self.new.mkdir()
        self.data = {
            "metadata": {"max_turns": 60, "criteria": "objective"},
            "results": [{
                "seed": 1, "map": "map_32", "grid_type": "hex",
                "p1": "V200", "p2": "V4", "result": "P2_Win", "turns": 20,
                "error": None,
                "metrics": [{"turn": 20, "p2_obj": {"zoc_area": 0}}],
                "thinking_times": {"2": [10, 30]},
                "action_counts": {"2": {"Infantry": 3}},
            }],
        }
        self.write(self.base, self.data)
        self.write(self.new, self.data)

    def write(self, directory, data):
        (directory / "seed1.json").write_text(json.dumps(data), encoding="utf-8")

    def pairs(self, **kwargs):
        return comparison.load_pairs(self.base, self.new, [1], "V4", **kwargs)

    def test_normal_wins_and_limit_decisions_are_separate(self):
        games = [
            {**self.data["results"][0], "result": result}
            for result in ["P2_Win", "P2_Win_MaxTurns", "P1_Win_MaxTurns", "Draw_MaxTurns"]
        ]
        totals = comparison.aggregate(games, 2)
        self.assertEqual(totals["outcomes"], {
            "win": 1, "limit_win": 1, "limit_loss": 1, "limit_draw": 1,
        })
        self.assertEqual(totals["thinking_mean"], 20)
        self.assertEqual(totals["production"]["Infantry"], 12)

    def test_missing_seed_is_an_error_not_a_smaller_denominator(self):
        with self.assertRaises(ValueError):
            comparison.load_pairs(self.base, self.new, [1, 2], "V4")

    def test_matchup_must_match_across_baseline_and_candidate(self):
        for key, value in [("map", "map_1"), ("grid_type", "square"), ("p1", "V1")]:
            with self.subTest(key=key):
                changed = copy.deepcopy(self.data)
                changed["results"][0][key] = value
                self.write(self.new, changed)
                with self.assertRaises(ValueError):
                    self.pairs()
        changed = copy.deepcopy(self.data)
        changed["metadata"]["max_turns"] = 30
        self.write(self.new, changed)
        with self.assertRaises(ValueError):
            self.pairs()

    def test_side_changes_are_not_silently_compared(self):
        changed = copy.deepcopy(self.data)
        changed["results"][0].update(p1="V4", p2="V200")
        self.write(self.new, changed)
        with self.assertRaises(ValueError):
            self.pairs()

    def test_same_version_match_requires_explicit_side(self):
        game = {"p1": "V4", "p2": "V4"}
        with self.assertRaises(ValueError):
            comparison.resolve_side(game, "V4")
        self.assertEqual(comparison.resolve_side(game, "V4", 2), 2)
        with self.assertRaises(ValueError):
            comparison.resolve_side(game, "V200", 2)

    def test_version_comparison_can_name_the_baseline_subject(self):
        changed = copy.deepcopy(self.data)
        changed["results"][0]["p2"] = "V3"
        self.write(self.base, changed)
        with self.assertRaises(ValueError):
            self.pairs()
        self.assertEqual(len(self.pairs(baseline_subject="V3")), 1)

    def test_invalid_games_are_not_counted_as_losses(self):
        for change in [
            lambda d: d["results"].append(copy.deepcopy(d["results"][0])),
            lambda d: d["results"][0].update(error="timeout"),
            lambda d: d["results"][0].update(result="Unknown"),
            lambda d: d["results"][0].update(seed=2),
            lambda d: d["metadata"].pop("max_turns"),
        ]:
            changed = copy.deepcopy(self.data)
            change(changed)
            self.write(self.new, changed)
            with self.assertRaises(ValueError):
                self.pairs()

    def test_missing_observations_are_not_zero_or_forward_filled(self):
        game = self.data["results"][0]
        self.assertEqual(comparison.objective_at(game, 2, 20), {"zoc_area": 0})
        self.assertEqual(comparison.objective_at(game, 2, 30), {})
        self.assertEqual(comparison.objective_at(game, 2, 15), {})
        self.assertEqual(comparison.display(None), "N/A")
        self.assertEqual(comparison.display(0), "0")
        unknown = {k: v for k, v in game.items() if k not in ("thinking_times", "action_counts")}
        totals = comparison.aggregate([unknown], 2)
        self.assertIsNone(totals["thinking_mean"])
        self.assertEqual(totals["missing_production"], 1)

    def test_cli_writes_utf8_and_refuses_to_overwrite(self):
        output = self.root / "comparison.md"
        command = [
            sys.executable, str(Path(comparison.__file__)), str(self.base), str(self.new),
            "--seeds", "1", "--output", str(output),
        ]
        result = subprocess.run(command, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_text(encoding="utf-8"), comparison.render(
            self.pairs(), self.base, self.new,
        ))
        original = output.read_bytes()
        self.assertEqual(subprocess.run(command, capture_output=True).returncode, 2)
        self.assertEqual(output.read_bytes(), original)
        (self.new / "seed1.json").unlink()
        self.assertEqual(subprocess.run(command[:-2], capture_output=True).returncode, 2)


if __name__ == "__main__":
    unittest.main()
