#!/usr/bin/env python3
"""AI再設計の構造指標を数える（効果見積りの「構造指標の推移」と同じ項目）。

- path_checks: 経路判定（`is_extended_at` / `ext_enabled` / `is_extended`）を含む行（定義を含む）
- extended_pairs: 親ファイルと同ディレクトリの `extended.rs` に同名で定義された非テスト関数の数
- extended_glob_reexports: `pub use ...extended::*` の数
- repair_calls: 修復・事後修正関数（`REPAIR_FUNCTIONS`）の非テスト呼び出し数（定義は除く）

テストモジュール（`#[cfg(test)]`）とテスト専用ファイルは除く。判定は行単位の正規表現で、
コメント行は数えない。

使い方:

    python scripts/measure_ai_structure.py --root engine/src/ai [--output out.json]
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from scripts.measure_ai_complexity import test_line_ranges  # noqa: E402

#: 修復・事後修正として数える関数（所属正規化・再接続・Roadmap再照合・DAG再準備・
#: 要求の0化・保存後の施設交換）。
REPAIR_FUNCTIONS = (
    "normalize_occupancy",
    "reconnect_v4_operation_fixed_point",
    "reconnect_current_v4_operations",
    "reconnect_unassigned_transport_waves",
    "reconcile_campaign_roadmap",
    "prepare_capital_route_topologies",
    "ensure_capital_route_node_operations",
    "clear_slot",
    "optimize_production_facility_assignments",
)

_PATH_CHECK = re.compile(r"\b(is_extended_at|ext_enabled|is_extended)\b")
_GLOB_REEXPORT = re.compile(r"^\s*pub use .*extended::\*")
_FN_DEF = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+|async\s+|unsafe\s+)*fn\s+(\w+)")
_REPAIR_CALL = re.compile(r"\b(" + "|".join(REPAIR_FUNCTIONS) + r")\s*\(")


def _code_lines(file: Path, ranges: list[tuple[float, float]]) -> list[tuple[int, str]]:
    """テスト範囲とコメント行を除いた (行番号, 行) の列。"""
    lines = []
    for number, line in enumerate(file.read_text(encoding="utf-8").splitlines(), start=1):
        if any(start <= number <= end for start, end in ranges):
            continue
        if line.lstrip().startswith("//"):
            continue
        lines.append((number, line))
    return lines


def _parent_of(extended: Path) -> Path | None:
    """`x/extended.rs` の親実装ファイル（`x.rs` または `x/mod.rs`）。"""
    directory = extended.parent
    for candidate in (directory.with_suffix(".rs"), directory / "mod.rs"):
        if candidate.exists():
            return candidate
    return None


def measure(root: Path) -> dict:
    files = sorted(root.rglob("*.rs"))
    ranges = test_line_ranges(files)
    lines = {file: _code_lines(file, ranges[str(file)]) for file in files}
    whole_test = {
        file for file in files if any(end == math.inf for _, end in ranges[str(file)])
    }

    path_checks = sum(
        1 for file in files for _, line in lines[file] if _PATH_CHECK.search(line)
    )
    glob_reexports = sum(
        1 for file in files for _, line in lines[file] if _GLOB_REEXPORT.match(line)
    )

    def defined(file: Path) -> set[str]:
        return {m.group(1) for _, line in lines[file] if (m := _FN_DEF.match(line))}

    pairs: dict[str, list[str]] = {}
    for extended in files:
        if extended.name != "extended.rs" or extended in whole_test:
            continue
        parent = _parent_of(extended)
        if parent is None:
            continue
        shared = sorted(defined(parent) & defined(extended))
        if shared:
            pairs[str(extended.relative_to(root))] = shared

    repair: dict[str, int] = {name: 0 for name in REPAIR_FUNCTIONS}
    for file in files:
        for _, line in lines[file]:
            if _FN_DEF.match(line):
                continue
            for match in _REPAIR_CALL.finditer(line):
                repair[match.group(1)] += 1

    return {
        "path_checks": path_checks,
        "extended_pairs": sum(len(names) for names in pairs.values()),
        "extended_glob_reexports": glob_reexports,
        "repair_calls": sum(repair.values()),
        "repair_calls_by_function": {k: v for k, v in repair.items() if v},
        "extended_pairs_by_file": pairs,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="AI再設計の構造指標を数える")
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    result = measure(args.root)
    text = json.dumps(result, ensure_ascii=False, indent=2)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text + "\n", encoding="utf-8")
    summary = {k: v for k, v in result.items() if not isinstance(v, dict)}
    print(json.dumps(summary, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
