#!/usr/bin/env python3
"""AIコードの複雑度を、関数ごとの「直接値」と「子込み値」に分けて計測する。

rust-code-analysis（RCA）の `-m -O json` 出力を入力にする。RCAの関数spaceの
cyclomatic / cognitive の `sum` は、その関数の中にあるクロージャ・入れ子関数の値を
含む（子込み値）。そのため「親関数自身の分岐の多さ」を見たいときは、子spaceの
`sum` を差し引いた直接値を使う。本スクリプトは両方を別の列として出力し、混同しない。

- 直接値（direct）: 関数spaceの値 − 直下の子space（クロージャ・入れ子関数）の値
- 子込み値（inclusive）: RCAの関数spaceの `sum` そのもの
- ABC、呼出深さ: 本スクリプトでは算出しない。RCAのRust ABCは未対応（0）であり、
  呼出グラフは同名method等を解決できないため、出力には "unsupported" と明記する。

使い方:

    # RCAを実行して計測（rust-code-analysis-cli がPATHにあること）
    python scripts/measure_ai_complexity.py --root engine/src/ai --output .tmp/complexity.json

    # 既に保存したRCAのJSON（1ファイル1行のJSON Lines、またはJSON配列）から計測
    python scripts/measure_ai_complexity.py --rca-json .tmp/rca.jsonl --output .tmp/complexity.json

出力JSONの `functions` は関数ごとの値、`summary` は全体の最大・P95・合計。
`--top N` を付けると、直接CCの大きい順に上位N件を標準出力へ表として出す。
"""

from __future__ import annotations

import argparse
import json
import math
import re
import shutil
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterable, Iterator

#: 計測しない指標と理由。出力に残し、0を実測値と誤読させない。
UNSUPPORTED_METRICS = {
    "abc": "RCAのRust ABCは未対応（常に0）。独自近似は校正前のため算出しない",
    "call_depth": "同名method・trait経由の呼出を解決できないため算出しない",
}

#: 関数として数えるspaceの種別。
FUNCTION_KINDS = {"function"}


class MeasureError(Exception):
    """入力の欠測・不正を表す。黙って0を返さず失敗させる。"""


@dataclass(frozen=True)
class FunctionMetrics:
    """関数1件の複雑度。"""

    file: str
    name: str
    start_line: int
    end_line: int
    direct_cyclomatic: float
    inclusive_cyclomatic: float
    direct_cognitive: float
    inclusive_cognitive: float
    #: 直下の子space（クロージャ・入れ子関数）の数
    child_spaces: int
    #: `#[cfg(test)]` のモジュール内にある関数か。RCA 0.0.25はRustの `mod` を
    #: spaceにしないため、ソースから求めた行範囲で判定する
    in_test_module: bool


def _metric_sum(space: dict[str, Any], metric: str, where: str) -> float:
    """spaceの指標 `sum` を取り出す。欠けていれば入力不正として失敗する。"""
    try:
        value = space["metrics"][metric]["sum"]
    except (KeyError, TypeError) as error:
        raise MeasureError(f"{where}: metrics.{metric}.sum がない") from error
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise MeasureError(f"{where}: metrics.{metric}.sum が数値ではない: {value!r}")
    if math.isnan(value) or value < 0:
        raise MeasureError(f"{where}: metrics.{metric}.sum が不正: {value!r}")
    return float(value)


#: 行範囲（両端を含む）。ファイル全体がテストなら (1, math.inf)。
LineRange = tuple[float, float]

_CFG_TEST = re.compile(r"^\s*#\[cfg\(test\)\]\s*$")
_MOD_INLINE = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*\{")
_MOD_FILE = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?mod\s+(\w+)\s*;")


def _count_braces(line: str) -> int:
    """1行の `{` と `}` の差。文字列リテラル・文字リテラル・行コメント内は数えない。"""
    depth = 0
    in_string = False
    index = 0
    while index < len(line):
        char = line[index]
        if in_string:
            if char == "\\":
                index += 2
                continue
            if char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "'" and index + 2 < len(line) and line[index + 2] == "'":
            index += 3
            continue
        elif line.startswith("//", index):
            break
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        index += 1
    return depth


def test_module_ranges(source: str) -> tuple[list[LineRange], list[str]]:
    """`#[cfg(test)]` の付いたインラインmodの行範囲と、別ファイルmodの名前を返す。"""
    lines = source.splitlines()
    ranges: list[LineRange] = []
    file_modules: list[str] = []
    index = 0
    while index < len(lines):
        if not _CFG_TEST.match(lines[index]):
            index += 1
            continue
        target = index + 1
        # 属性やdocコメントが続く場合は読み飛ばす
        while target < len(lines) and lines[target].strip().startswith(("#[", "///", "//")):
            target += 1
        if target >= len(lines):
            break
        declared = _MOD_FILE.match(lines[target])
        if declared:
            file_modules.append(declared.group(3))
            index = target + 1
            continue
        if not _MOD_INLINE.match(lines[target]):
            index = target
            continue
        # mod行の `{` から、対応する `}` で深さが0へ戻る行までを範囲とする
        depth = 0
        end = target
        for end in range(target, len(lines)):
            depth += _count_braces(lines[end])
            if depth <= 0:
                break
        ranges.append((target + 1, end + 1))
        index = end + 1
    return ranges, file_modules


def _module_dir(file: Path) -> Path:
    """`mod x;` が解決されるディレクトリ。mod.rs/lib.rs/main.rs はそのディレクトリ。"""
    if file.name in {"mod.rs", "lib.rs", "main.rs"}:
        return file.parent
    return file.parent / file.stem


def test_line_ranges(files: Iterable[Path]) -> dict[str, list[LineRange]]:
    """ファイルごとのテスト行範囲。`#[cfg(test)] mod x;` の先はファイル全体をテストとする。"""
    files = list(files)
    result: dict[str, list[LineRange]] = {}
    whole_test_files: set[Path] = set()
    for file in files:
        ranges, file_modules = test_module_ranges(file.read_text(encoding="utf-8"))
        result[str(file)] = ranges
        for name in file_modules:
            base = _module_dir(file)
            for candidate in (base / f"{name}.rs", base / name / "mod.rs"):
                whole_test_files.add(candidate.resolve())
    for file in files:
        if file.resolve() in whole_test_files:
            result[str(file)] = [(1, math.inf)]
    # テストファイル配下のサブモジュール（scenario_tests/ 等）もテストとする
    for file in files:
        if any(
            parent == test_file.parent / test_file.stem
            for test_file in whole_test_files
            for parent in file.resolve().parents
        ):
            result[str(file)] = [(1, math.inf)]
    return result


def _in_ranges(line: int, ranges: Iterable[LineRange]) -> bool:
    return any(start <= line <= end for start, end in ranges)


def _walk_functions(
    space: dict[str, Any], file: str, test_ranges: list[LineRange]
) -> Iterator[FunctionMetrics]:
    """space木を深さ優先にたどり、関数spaceの直接値・子込み値を求める。"""
    children = space.get("spaces") or []
    if not isinstance(children, list):
        raise MeasureError(f"{file}: spaces が配列ではない")
    kind = space.get("kind")
    name = space.get("name") or "<anonymous>"
    if kind in FUNCTION_KINDS:
        where = f"{file}:{space.get('start_line', '?')} {name}"
        inclusive_cc = _metric_sum(space, "cyclomatic", where)
        inclusive_cog = _metric_sum(space, "cognitive", where)
        child_cc = sum(
            _metric_sum(child, "cyclomatic", where) for child in children
        )
        child_cog = sum(_metric_sum(child, "cognitive", where) for child in children)
        direct_cc = inclusive_cc - child_cc
        direct_cog = inclusive_cog - child_cog
        if direct_cc < 0 or direct_cog < 0:
            raise MeasureError(
                f"{where}: 子spaceの合計が親の値を超える（cc {inclusive_cc} < {child_cc} "
                f"または cognitive {inclusive_cog} < {child_cog}）"
            )
        yield FunctionMetrics(
            file=file,
            name=name,
            start_line=int(space.get("start_line", 0)),
            end_line=int(space.get("end_line", 0)),
            direct_cyclomatic=direct_cc,
            inclusive_cyclomatic=inclusive_cc,
            direct_cognitive=direct_cog,
            inclusive_cognitive=inclusive_cog,
            child_spaces=len(children),
            in_test_module=_in_ranges(int(space.get("start_line", 0)), test_ranges),
        )
    for child in children:
        if not isinstance(child, dict):
            raise MeasureError(f"{file}: spaceがオブジェクトではない")
        yield from _walk_functions(child, file, test_ranges)


def functions_from_unit(
    unit: dict[str, Any], test_ranges: list[LineRange] | None = None
) -> list[FunctionMetrics]:
    """RCAのファイル単位JSONから関数一覧を求める。"""
    if not isinstance(unit, dict) or "name" not in unit:
        raise MeasureError("RCAのファイル単位JSONに name がない")
    return list(_walk_functions(unit, str(unit["name"]), test_ranges or []))


def load_rca_units(path: Path) -> list[dict[str, Any]]:
    """保存したRCA出力（JSON配列、または1行1ファイルのJSON Lines）を読む。"""
    text = path.read_text(encoding="utf-8")
    stripped = text.strip()
    if not stripped:
        raise MeasureError(f"{path}: 空のRCA出力")
    if stripped.startswith("["):
        try:
            units = json.loads(stripped)
        except json.JSONDecodeError as error:
            raise MeasureError(f"{path}: 不正なJSON: {error}") from error
        if not isinstance(units, list):
            raise MeasureError(f"{path}: JSON配列ではない")
        return units
    units = []
    for number, line in enumerate(text.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            units.append(json.loads(line))
        except json.JSONDecodeError as error:
            raise MeasureError(f"{path}:{number}: 不正なJSON行: {error}") from error
    return units


def run_rca(root: Path) -> list[dict[str, Any]]:
    """rust-code-analysis-cli を実行し、ファイル単位のJSONを返す。"""
    executable = shutil.which("rust-code-analysis-cli")
    if executable is None:
        raise MeasureError(
            "rust-code-analysis-cli が見つからない。"
            "`cargo install rust-code-analysis-cli --locked` で導入するか、"
            "保存済みの出力を --rca-json で渡す"
        )
    files = sorted(root.rglob("*.rs"))
    if not files:
        raise MeasureError(f"{root}: .rs ファイルがない")
    units = []
    for file in files:
        completed = subprocess.run(
            [executable, "-m", "-O", "json", "-p", str(file)],
            check=False,
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            raise MeasureError(f"{file}: RCAが失敗: {completed.stderr.strip()}")
        output = completed.stdout.strip()
        if not output:
            raise MeasureError(f"{file}: RCAの出力が空")
        units.append(json.loads(output))
    return units


def percentile(values: list[float], fraction: float) -> float:
    """最近傍順位法のパーセンタイル。空なら失敗する。"""
    if not values:
        raise MeasureError("関数が1件もない")
    ordered = sorted(values)
    rank = max(1, math.ceil(fraction * len(ordered)))
    return ordered[rank - 1]


def summarize(functions: list[FunctionMetrics]) -> dict[str, Any]:
    """全体の最大・P95・合計を、直接値と子込み値で別々に求める。"""
    summary: dict[str, Any] = {"function_count": len(functions)}
    for field in (
        "direct_cyclomatic",
        "inclusive_cyclomatic",
        "direct_cognitive",
        "inclusive_cognitive",
    ):
        values = [getattr(function, field) for function in functions]
        summary[field] = {
            "max": max(values) if values else 0.0,
            "p95": percentile(values, 0.95) if values else 0.0,
            # 合計は直接値だけが重複のない総量になる。子込み値の合計は
            # クロージャを親と子で二重に数えるので参考値。
            "total": sum(values),
        }
    return summary


def measure(
    units: Iterable[dict[str, Any]],
    include_tests: bool,
    test_ranges: dict[str, list[LineRange]] | None = None,
) -> dict[str, Any]:
    """RCAのファイル単位出力から、関数ごとの値と全体集計を作る。

    `test_ranges` はファイル名（RCAの `name`）ごとの `#[cfg(test)]` 行範囲。
    指定が無いファイルはソースが読めれば読み、読めなければテスト判定をしない。
    """
    test_ranges = dict(test_ranges or {})
    functions = []
    for unit in units:
        name = str(unit.get("name", "")) if isinstance(unit, dict) else ""
        if name not in test_ranges and Path(name).is_file():
            test_ranges.update(test_line_ranges([Path(name)]))
        functions.extend(
            function
            for function in functions_from_unit(unit, test_ranges.get(name, []))
            if include_tests or not function.in_test_module
        )
    if not functions:
        raise MeasureError("計測対象の関数が1件もない")
    functions.sort(key=lambda function: (function.file, function.start_line))
    return {
        "definition": {
            "direct": "関数spaceのsum − 直下の子spaceのsum",
            "inclusive": "RCAの関数spaceのsum（クロージャ・入れ子関数を含む）",
            "tests_included": include_tests,
        },
        "unsupported": UNSUPPORTED_METRICS,
        "summary": summarize(functions),
        "functions": [asdict(function) for function in functions],
    }


def format_top(result: dict[str, Any], top: int) -> str:
    """直接CCの大きい順に上位を表にする。"""
    rows = sorted(
        result["functions"],
        key=lambda function: (-function["direct_cyclomatic"], function["file"]),
    )[:top]
    lines = ["| 直接CC | 子込みCC | 直接認知 | 子込み認知 | 関数 |", "|---:|---:|---:|---:|---|"]
    for row in rows:
        lines.append(
            f"| {row['direct_cyclomatic']:.0f} | {row['inclusive_cyclomatic']:.0f} "
            f"| {row['direct_cognitive']:.0f} | {row['inclusive_cognitive']:.0f} "
            f"| {row['file']}:{row['start_line']} {row['name']} |"
        )
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--root", type=Path, help="計測する .rs のルートディレクトリ")
    source.add_argument("--rca-json", type=Path, help="保存済みのRCA出力")
    parser.add_argument("--output", type=Path, required=True, help="出力JSON")
    parser.add_argument(
        "--include-tests", action="store_true", help="tests モジュール内の関数も含める"
    )
    parser.add_argument("--top", type=int, default=0, help="直接CC上位N件を表示")
    args = parser.parse_args(argv)
    try:
        if args.root:
            units = run_rca(args.root)
            ranges = test_line_ranges(sorted(args.root.rglob("*.rs")))
        else:
            units = load_rca_units(args.rca_json)
            ranges = None
        result = measure(units, include_tests=args.include_tests, test_ranges=ranges)
    except MeasureError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    if args.top:
        print(format_top(result, args.top))
    return 0


if __name__ == "__main__":
    sys.exit(main())
