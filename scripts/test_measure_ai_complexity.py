"""measure_ai_complexity.py の単体テスト。

RCAを起動せず、手計算した値を持つ合成spaceで直接値・子込み値の分離と、
不正入力を失敗として扱うことを確認する。
"""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from scripts import measure_ai_complexity as measure


def space(kind, name, cyclomatic, cognitive, children=(), start=1, end=2):
    """RCA形式のspaceを1件作る。"""
    return {
        "kind": kind,
        "name": name,
        "start_line": start,
        "end_line": end,
        "metrics": {
            "cyclomatic": {"sum": cyclomatic},
            "cognitive": {"sum": cognitive},
        },
        "spaces": list(children),
    }


def unit(*children):
    """1ファイル分のunit space。"""
    total_cc = sum(child["metrics"]["cyclomatic"]["sum"] for child in children)
    total_cog = sum(child["metrics"]["cognitive"]["sum"] for child in children)
    return {
        **space("unit", "engine/src/ai/sample.rs", total_cc, total_cog, children),
    }


class DirectAndInclusiveTest(unittest.TestCase):
    def test_closure_is_subtracted_from_parent(self):
        # 親: if 1つ（base 1 + 1）＋ 子クロージャ: match 2腕（base 1 + 1）
        # RCAの親sumは子込みで 2 + 2 = 4
        closure = space("function", "<anonymous>", 2, 1, start=3, end=4)
        parent = space("function", "plan", 4, 3, [closure], start=1, end=10)
        functions = measure.functions_from_unit(unit(parent))
        by_name = {(f.name, f.start_line): f for f in functions}
        plan = by_name[("plan", 1)]
        self.assertEqual(plan.inclusive_cyclomatic, 4)
        self.assertEqual(plan.direct_cyclomatic, 2)
        self.assertEqual(plan.inclusive_cognitive, 3)
        self.assertEqual(plan.direct_cognitive, 2)
        self.assertEqual(plan.child_spaces, 1)
        anonymous = by_name[("<anonymous>", 3)]
        self.assertEqual(anonymous.direct_cyclomatic, 2)

    def test_methods_with_same_name_stay_separate(self):
        # 同名methodを別implで定義しても、位置で区別して二重計上しない
        first = space("function", "new", 1, 0, start=5, end=6)
        second = space("function", "new", 3, 2, start=20, end=30)
        impls = [
            space("impl", "A", 1, 0, [first], start=4, end=7),
            space("impl", "B", 3, 2, [second], start=19, end=31),
        ]
        result = measure.measure([unit(*impls)], include_tests=False)
        self.assertEqual(result["summary"]["function_count"], 2)
        self.assertEqual(result["summary"]["direct_cyclomatic"]["total"], 4)
        self.assertEqual(result["summary"]["direct_cyclomatic"]["max"], 3)

    def test_test_module_is_excluded_unless_requested(self):
        # RCA 0.0.25 は mod をspaceにしないため、テスト関数はunit直下に並ぶ
        body = space("function", "body", 2, 1, start=1, end=3)
        test_fn = space("function", "fixture", 7, 9, start=6, end=8)
        units = [unit(body, test_fn)]
        ranges = {"engine/src/ai/sample.rs": [(5, 9)]}
        without = measure.measure(units, include_tests=False, test_ranges=ranges)
        self.assertEqual(without["summary"]["function_count"], 1)
        with_tests = measure.measure(units, include_tests=True, test_ranges=ranges)
        self.assertEqual(with_tests["summary"]["function_count"], 2)

    def test_cfg_test_module_ranges_from_source(self):
        source = "\n".join(
            [
                "fn body() {}",  # 1
                "#[cfg(test)]",  # 2
                "mod tests {",  # 3
                '    const S: &str = "}";',  # 4 文字列内の } は数えない
                "    fn a() { if true { } } // }",  # 5
                "}",  # 6
                "fn after() {}",  # 7
                "#[cfg(test)]",  # 8
                "mod extra_tests;",  # 9
            ]
        )
        ranges, files = measure.test_module_ranges(source)
        self.assertEqual(ranges, [(3, 6)])
        self.assertEqual(files, ["extra_tests"])

    def test_cfg_test_file_module_is_whole_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ai").mkdir()
            parent = root / "ai" / "mod.rs"
            parent.write_text("#[cfg(test)]\nmod scenario_tests;\nfn body() {}\n", encoding="utf-8")
            child = root / "ai" / "scenario_tests.rs"
            child.write_text("fn fixture() {}\n", encoding="utf-8")
            nested_dir = root / "ai" / "scenario_tests"
            nested_dir.mkdir()
            nested = nested_dir / "extended.rs"
            nested.write_text("fn more() {}\n", encoding="utf-8")
            ranges = measure.test_line_ranges([parent, child, nested])
            self.assertEqual(ranges[str(parent)], [])
            self.assertEqual(ranges[str(child)][0][0], 1)
            self.assertEqual(ranges[str(nested)][0][0], 1)

    def test_unsupported_metrics_are_named(self):
        body = space("function", "body", 1, 0)
        result = measure.measure([unit(body)], include_tests=False)
        self.assertIn("abc", result["unsupported"])
        self.assertIn("call_depth", result["unsupported"])

    def test_percentile_uses_nearest_rank(self):
        self.assertEqual(measure.percentile([1, 2, 3, 4, 100], 0.95), 100)
        self.assertEqual(measure.percentile([5], 0.95), 5)


class InvalidInputTest(unittest.TestCase):
    def test_missing_metric_fails(self):
        broken = space("function", "broken", 1, 0)
        del broken["metrics"]["cognitive"]
        with self.assertRaises(measure.MeasureError):
            measure.functions_from_unit(unit_with(broken))

    def test_children_exceeding_parent_fails(self):
        child = space("function", "<anonymous>", 5, 0)
        parent = space("function", "parent", 2, 0, [child])
        with self.assertRaises(measure.MeasureError):
            measure.functions_from_unit(unit_with(parent))

    def test_no_function_fails(self):
        with self.assertRaises(measure.MeasureError):
            measure.measure([unit()], include_tests=False)

    def test_empty_and_malformed_files_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            empty = Path(directory) / "empty.jsonl"
            empty.write_text("", encoding="utf-8")
            with self.assertRaises(measure.MeasureError):
                measure.load_rca_units(empty)
            malformed = Path(directory) / "bad.jsonl"
            malformed.write_text('{"name": "a"}\n{not json\n', encoding="utf-8")
            with self.assertRaises(measure.MeasureError):
                measure.load_rca_units(malformed)

    def test_json_lines_round_trip_through_main(self):
        body = space("function", "body", 3, 2)
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "rca.jsonl"
            source.write_text(json.dumps(unit(body)) + "\n", encoding="utf-8")
            output = Path(directory) / "out.json"
            status = measure.main(["--rca-json", str(source), "--output", str(output)])
            self.assertEqual(status, 0)
            result = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(result["functions"][0]["direct_cyclomatic"], 3)


def unit_with(child):
    """指標が欠けたspaceも包めるよう、unit自身の値は固定値にする。"""
    wrapper = space("unit", "engine/src/ai/sample.rs", 0, 0, [child])
    return wrapper


if __name__ == "__main__":
    unittest.main()
