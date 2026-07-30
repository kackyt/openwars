# Issue #58 Evaluation and Diagnosis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a reproducible fixed-seed evaluation path for Issue #58, collect occupation/production/combat/invasion evidence, and produce the baseline and root-cause analysis that determines the separate AI-fix plan.

**Architecture:** Keep game rules in `engine`, expose structured evaluation-only telemetry through `mcp-server`, and isolate Issue #58 aggregation/reporting in a new Python module. Run every seed in both player orders, measure each round only after both players finish, preserve raw JSON as the source of truth, and derive Markdown/analysis artifacts from it.

**Tech Stack:** Rust 2024, bevy_ecs 0.15.2, serde/serde_json, rmcp, Python 3 standard library (`argparse`, `dataclasses`, `hashlib`, `json`, `statistics`, `unittest`, `unittest.mock`).

## Global Constraints

- Add Japanese comments that explain non-obvious game/evaluation logic.
- Do not add map_3 coordinates, player-order branches, or seed-specific AI behavior.
- Do not modify V1 transport behavior or reimplement Load/Transit/Drop rules.
- Issue #58 runs require at least four unique seeds; the canonical set is `58001,58002,58003,58004`.
- T30 means the board after both P1 and P2 have completed their 30th actions.
- ZOC requires `V3 > V2`; income requires `V3 >= V2`.
- Never overwrite `matchup_report.md` in Issue #58 mode.
- Store baseline artifacts under `benchmarks/issue-58/` and include commit SHA, dirty state, command, seeds, trial counts, and evaluator/MCP content hashes.
- Do not create GitHub child issues; write local drafts to `benchmarks/issue-58/child-issue-drafts.md`.
- Do not commit unless the user explicitly authorizes it. The checkpoint steps below inspect diffs but do not create commits under the current instruction.
- Required quality gates: `python -m unittest scripts.test_eval_matchup scripts.test_eval_issue58`, `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo fmt --all -- --check`.

---

## File Structure

### Create

- `scripts/eval_issue58.py`
  - Owns Issue #58 seed validation, per-game analysis, hand-order aggregation, acceptance criteria, metadata, Markdown rendering, and JSON-safe serialization.
- `scripts/test_eval_issue58.py`
  - Unit tests for seed validation, equal-income semantics, T15 trend rules, external-property acquisition, error buckets, production investment, occupation throughput, and Battleship ROI.
- `benchmarks/issue-58/seeds.txt`
  - Canonical fixed seed set.
- `benchmarks/issue-58/analysis.md`
  - Evidence table classifying each hypothesis as `confirmed`, `rejected`, or `inconclusive` after baseline execution.
- `benchmarks/issue-58/child-issue-drafts.md`
  - One local issue draft per confirmed cause.
- `benchmarks/issue-58/baseline-$BASE_SHA.md`
  - Human-readable baseline report generated from raw JSON.
- `benchmarks/issue-58/baseline-$BASE_SHA.json`
  - Raw reproducible baseline result.

### Modify

- `scripts/eval_matchup.py:95-292`
  - Replace pre-action metrics with round-end metrics and add dependency injection for MCP calls.
- `scripts/eval_matchup.py:475-553`
  - Reuse shared moving-average behavior while preserving existing objective criteria.
- `scripts/eval_matchup.py:671-792`
  - Add `issue58`, `--seeds`, `--json-output`, metadata capture, safe output validation, and fixed-seed match scheduling.
- `scripts/test_eval_matchup.py`
  - Add orchestration tests for paired seed execution and the T30 boundary while retaining Issue #54 tests.
- `mcp-server/src/invasion_trace.rs`
  - Add structured unit snapshots, production events, rich combat traces, and pure damage-value calculation.
- `mcp-server/src/main.rs:313-442`
  - Add `cost` and `can_capture` to board-state units.
- `mcp-server/src/main.rs:448-519`
  - Pass before-action unit snapshots into the trace collector.

### Deferred to the second plan

- `engine/src/ai/strategy.rs`
- `engine/src/ai/objectives.rs`
- `engine/src/ai/squad.rs`
- `engine/src/ai/beam_search.rs`
- `engine/src/ai/production.rs`

These files must not change in this plan. Their exact changes are selected only after `analysis.md` identifies confirmed causes.

---

### Task 1: Add Issue #58 seed protocol and run metadata

**Files:**
- Create: `scripts/eval_issue58.py`
- Create: `scripts/test_eval_issue58.py`

**Interfaces:**
- Consumes: CLI strings and repository paths.
- Produces:
  - `parse_seed_list(raw: str) -> tuple[int, ...]`
  - `validate_issue58_run(maps: tuple[str, ...], subject: str, baseline: str, max_turns: int, seeds: tuple[int, ...], markdown_output: str, json_output: str) -> None`
  - `collect_run_metadata(argv: list[str], seeds: tuple[int, ...], evaluator_paths: tuple[str, ...], mcp_paths: tuple[str, ...]) -> dict`
  - `sha256_files(paths: tuple[str, ...]) -> str`

- [ ] **Step 1: Write failing seed parser tests**

Add to `scripts/test_eval_issue58.py`:

```python
import unittest

from scripts.eval_issue58 import parse_seed_list, validate_issue58_run


class Issue58SeedProtocolTests(unittest.TestCase):
    def test_parse_seed_list_preserves_explicit_order(self):
        self.assertEqual((58001, 58002, 58003, 58004), parse_seed_list("58001,58002,58003,58004"))

    def test_parse_seed_list_rejects_duplicate_seed(self):
        with self.assertRaisesRegex(ValueError, "seed must be unique"):
            parse_seed_list("58001,58002,58001,58004")

    def test_issue58_requires_four_seeds(self):
        with self.assertRaisesRegex(ValueError, "at least 4"):
            validate_issue58_run(
                maps=("map_3",),
                subject="V3",
                baseline="V2",
                max_turns=30,
                seeds=(1, 2, 3),
                markdown_output="benchmarks/issue-58/baseline.md",
                json_output="benchmarks/issue-58/baseline.json",
            )

    def test_issue58_rejects_matchup_report_output(self):
        with self.assertRaisesRegex(ValueError, "matchup_report.md"):
            validate_issue58_run(
                maps=("map_3",),
                subject="V3",
                baseline="V2",
                max_turns=30,
                seeds=(1, 2, 3, 4),
                markdown_output="matchup_report.md",
                json_output="benchmarks/issue-58/baseline.json",
            )
```

- [ ] **Step 2: Run the focused test and verify failure**

Run:

```text
python -m unittest scripts.test_eval_issue58.Issue58SeedProtocolTests
```

Expected: `ModuleNotFoundError` for `scripts.eval_issue58`.

- [ ] **Step 3: Implement seed parsing and validation**

Create `scripts/eval_issue58.py` with:

```python
from __future__ import annotations

import hashlib
import json
import os
import subprocess
from pathlib import Path


def parse_seed_list(raw: str) -> tuple[int, ...]:
    seeds = tuple(int(part.strip()) for part in raw.split(",") if part.strip())
    if len(set(seeds)) != len(seeds):
        raise ValueError("each seed must be unique")
    return seeds


def validate_issue58_run(
    maps: tuple[str, ...],
    subject: str,
    baseline: str,
    max_turns: int,
    seeds: tuple[int, ...],
    markdown_output: str,
    json_output: str,
) -> None:
    if "map_3" not in maps:
        raise ValueError("Issue #58 evaluation must include map_3")
    if subject != "V3" or baseline != "V2":
        raise ValueError("Issue #58 requires subject V3 and baseline V2")
    if max_turns != 30:
        raise ValueError("Issue #58 requires max_turns=30")
    if len(seeds) < 4:
        raise ValueError("Issue #58 requires at least 4 unique seeds")
    if len(set(seeds)) != len(seeds):
        raise ValueError("each seed must be unique")
    if Path(markdown_output).name == "matchup_report.md":
        raise ValueError("Issue #58 must not overwrite matchup_report.md")
    if Path(markdown_output).resolve() == Path(json_output).resolve():
        raise ValueError("Markdown and JSON outputs must be different files")


def sha256_files(paths: tuple[str, ...]) -> str:
    digest = hashlib.sha256()
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
    dirty = bool(subprocess.run(
        ["git", "status", "--porcelain"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip())
    return {
        "commit_sha": commit_sha,
        "working_tree_dirty": dirty,
        "command": argv,
        "seeds": list(seeds),
        "games_per_order": len(seeds),
        "evaluator_sha256": sha256_files(evaluator_paths),
        "mcp_sha256": sha256_files(mcp_paths),
    }
```

Keep imports limited to the Python standard library.

- [ ] **Step 4: Run the focused test and verify pass**

Run:

```text
python -m unittest scripts.test_eval_issue58.Issue58SeedProtocolTests
```

Expected: 4 tests pass.

- [ ] **Step 5: Add metadata tests without invoking real Git**

Add:

```python
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from scripts.eval_issue58 import collect_run_metadata, sha256_files


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
```

- [ ] **Step 6: Run all new module tests**

Run:

```text
python -m unittest scripts.test_eval_issue58
```

Expected: all tests pass.

- [ ] **Step 7: Inspect the checkpoint without committing**

Run:

```text
git diff --check -- scripts/eval_issue58.py scripts/test_eval_issue58.py
git diff --stat -- scripts/eval_issue58.py scripts/test_eval_issue58.py
```

Expected: no whitespace errors; only the two intended files are listed.

---

### Task 2: Measure only completed rounds and pair both orders by seed

**Files:**
- Modify: `scripts/eval_matchup.py:95-292`
- Modify: `scripts/eval_matchup.py:671-765`
- Modify: `scripts/test_eval_matchup.py`

**Interfaces:**
- Consumes: existing MCP `call_tool` responses and `parse_seed_list` from Task 1.
- Produces:
  - `collect_round_metrics(tool_caller, state: dict, round_number: int) -> dict`
  - `build_match_specs(maps: tuple[str, ...], subject: str, baseline: str, seeds: tuple[int, ...]) -> list[dict]`
  - `run_single_game(..., tool_caller=None)` returning `seed`, round-end `metrics`, `strategic_history`, `final_state`, and `error`.

- [ ] **Step 1: Write failing paired-seed scheduling tests**

Append to `scripts/test_eval_matchup.py`:

```python
from scripts.eval_matchup import build_match_specs


class MatchSchedulingTests(unittest.TestCase):
    def test_build_match_specs_runs_each_seed_in_both_orders(self):
        specs = build_match_specs(("map_3",), "V3", "V2", (11, 22))
        self.assertEqual([
            {"map": "map_3", "p1": "V3", "p2": "V2", "seed": 11},
            {"map": "map_3", "p1": "V2", "p2": "V3", "seed": 11},
            {"map": "map_3", "p1": "V3", "p2": "V2", "seed": 22},
            {"map": "map_3", "p1": "V2", "p2": "V3", "seed": 22},
        ], specs)
```

- [ ] **Step 2: Run the scheduling test and verify failure**

Run:

```text
python -m unittest scripts.test_eval_matchup.MatchSchedulingTests
```

Expected: import failure for `build_match_specs`.

- [ ] **Step 3: Implement deterministic match specification generation**

Add near the top-level helpers in `scripts/eval_matchup.py`:

```python
def build_match_specs(maps, subject, baseline, seeds):
    specs = []
    for map_name in maps:
        for seed in seeds:
            specs.append({"map": map_name, "p1": subject, "p2": baseline, "seed": seed})
            specs.append({"map": map_name, "p1": baseline, "p2": subject, "seed": seed})
    return specs
```

Use the generated specs in both TUI and batch paths instead of duplicating the two-order loops.

- [ ] **Step 4: Run the scheduling test and verify pass**

Run:

```text
python -m unittest scripts.test_eval_matchup.MatchSchedulingTests
```

Expected: pass.

- [ ] **Step 5: Write a failing completed-round boundary test**

Add a fake MCP caller that records two simulations before the first metric collection:

```python
from scripts.eval_matchup import run_single_game


class CompletedRoundTests(unittest.TestCase):
    def test_t30_snapshot_is_taken_after_both_players_finish(self):
        calls = []
        active_index = 0
        completed_actions = 0

        def tool(name, arguments=None, req_id=1):
            nonlocal active_index, completed_actions
            calls.append(name)
            if name == "load_map" or name == "set_player_ai_version":
                return {}
            if name == "get_board_state":
                return {
                    "active_player_index": active_index,
                    "players": [
                        {"player_id": 1, "property_count": 1, "unit_cost": 1000, "funds": 0},
                        {"player_id": 2, "property_count": 1, "unit_cost": 1000, "funds": 0},
                    ],
                    "properties": [],
                    "units": [],
                    "game_over": None,
                }
            if name == "evaluate_board":
                return {"score": 0, "subjective_metrics": {}, "objective_metrics": {
                    "zoc_area": completed_actions,
                    "income_per_turn": 1000,
                    "owned_properties": 1,
                }}
            if name == "simulate_ai_turn":
                completed_actions += 1
                active_index = 1 - active_index
                return {"actions_taken": [], "invasion_events": [], "transport_squads": []}
            raise AssertionError(name)

        result = run_single_game("map_3", "V3", "V2", 30, seed=7, tool_caller=tool)
        self.assertEqual(30, len(result["metrics"]))
        self.assertEqual(60, result["metrics"][-1]["p1_obj"]["zoc_area"])
        self.assertEqual(60, completed_actions)
```

- [ ] **Step 6: Run the boundary test and verify failure**

Run:

```text
python -m unittest scripts.test_eval_matchup.CompletedRoundTests
```

Expected: failure because `run_single_game` does not accept `tool_caller` and currently records pre-action metrics.

- [ ] **Step 7: Extract round-end collection and restructure `run_single_game`**

Change the signature to:

```python
def run_single_game(
    map_name,
    p1_ver,
    p2_ver,
    max_turns,
    seed=None,
    ui_callback=None,
    tool_caller=None,
):
    tool = tool_caller or call_tool
```

Replace direct `call_tool` calls inside this function with `tool`. Add:

```python
def collect_round_metrics(tool, state, round_number):
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
        metric[f"{side}_score"] = evaluation.get("score", 0)
        metric[f"{side}_subj"] = evaluation.get("subjective_metrics", {})
        metric[f"{side}_obj"] = evaluation.get("objective_metrics", {})
    for player in state.get("players", []):
        side = "p1" if player.get("player_id") == 1 else "p2"
        metric[f"{side}_props"] = player.get("property_count", 0)
        metric[f"{side}_units"] = player.get("unit_cost", 0)
        metric[f"{side}_funds"] = player.get("funds", 0)
        metric[f"{side}_abs_score"] = (
            metric[f"{side}_props"] * 20000 + metric[f"{side}_units"]
        )
    return metric
```

Use this round structure:

```python
for round_number in range(1, max_turns + 1):
    for _ in range(2):
        state = tool("get_board_state")
        if state.get("game_over"):
            return finish_game(...)
        current_player = state["players"][state.get("active_player_index", 0)]["player_id"]
        ai_result = tool("simulate_ai_turn")
        # Preserve timing, actions, invasion events, and transport history here.
        post_action_state = tool("get_board_state")
        if post_action_state.get("game_over"):
            metrics.append(collect_round_metrics(tool, post_action_state, round_number))
            return finish_game(...)
    round_state = tool("get_board_state")
    metrics.append(collect_round_metrics(tool, round_state, round_number))
    strategic_history.append({
        "turn": round_number,
        "properties": round_state.get("properties", []),
        "units": round_state.get("units", []),
        "transport_squads": round_state.get("transport_squads", []),
    })
```

Create one local result-builder helper so winner, draw, max-turn, and error returns contain the same keys. Include `seed` in every result.

- [ ] **Step 8: Run existing and new evaluator tests**

Run:

```text
python -m unittest scripts.test_eval_matchup
```

Expected: Issue #54 tests and the new scheduling/boundary tests pass.

- [ ] **Step 9: Inspect the checkpoint without committing**

Run:

```text
git diff --check -- scripts/eval_matchup.py scripts/test_eval_matchup.py
git diff --stat -- scripts/eval_matchup.py scripts/test_eval_matchup.py
```

Expected: no whitespace errors.

---

### Task 3: Add structured production and combat telemetry to the MCP adapter

**Files:**
- Modify: `mcp-server/src/invasion_trace.rs`
- Modify: `mcp-server/src/main.rs:313-442`
- Modify: `mcp-server/src/main.rs:448-519`

**Interfaces:**
- Consumes: `UnitLoadedEvent`, `UnitUnloadedEvent`, `UnitAttackedEvent`, `PropertyCaptureProgressedEvent`, `UnitProducedEvent`, ECS `Faction`, `GridPosition`, `Health`, and `UnitStats`.
- Produces:
  - `UnitTraceSnapshot`
  - `snapshot_units(world: &mut World) -> HashMap<u64, UnitTraceSnapshot>`
  - `calculate_damage_value(before: &UnitTraceSnapshot, after: Option<&UnitTraceSnapshot>) -> i64`
  - `InvasionEvent::UnitProduced`
  - enriched `InvasionEvent::UnitAttacked`

- [ ] **Step 1: Write failing pure damage calculation tests**

At the end of `mcp-server/src/invasion_trace.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use engine::resources::UnitType;

    fn snapshot(health: u32, cost: u32) -> UnitTraceSnapshot {
        UnitTraceSnapshot {
            player_id: 1,
            unit_type: UnitType::Battleship,
            health,
            max_health: 100,
            cost,
            can_capture: false,
            position: GridPosition { x: 1, y: 1 },
        }
    }

    #[test]
    fn damage_value_uses_hp_loss_and_unit_cost() {
        let before = snapshot(100, 28_000);
        let after = snapshot(75, 28_000);
        assert_eq!(7_000, calculate_damage_value(&before, Some(&after)));
    }

    #[test]
    fn destroyed_unit_counts_remaining_hp_as_loss() {
        let before = snapshot(40, 28_000);
        assert_eq!(11_200, calculate_damage_value(&before, None));
    }

    #[test]
    fn healing_or_unchanged_hp_never_creates_negative_damage() {
        let before = snapshot(50, 28_000);
        let after = snapshot(80, 28_000);
        assert_eq!(0, calculate_damage_value(&before, Some(&after)));
    }
}
```

- [ ] **Step 2: Run the MCP test and verify failure**

Run:

```text
cargo test -p mcp-server invasion_trace::tests
```

Expected: compile failure because `UnitTraceSnapshot` and `calculate_damage_value` do not exist.

- [ ] **Step 3: Define snapshots and damage calculation**

Add imports for `Health`, `UnitStats`, `UnitProducedEvent`, and `UnitType`. Define:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitTraceSnapshot {
    pub player_id: u32,
    pub unit_type: UnitType,
    pub health: u32,
    pub max_health: u32,
    pub cost: u32,
    pub can_capture: bool,
    pub position: GridPosition,
}

pub fn calculate_damage_value(
    before: &UnitTraceSnapshot,
    after: Option<&UnitTraceSnapshot>,
) -> i64 {
    let after_health = after.map_or(0, |snapshot| snapshot.health);
    let lost_hp = before.health.saturating_sub(after_health);
    i64::from(before.cost) * i64::from(lost_hp) / i64::from(before.max_health.max(1))
}

pub fn snapshot_units(world: &mut World) -> HashMap<u64, UnitTraceSnapshot> {
    let mut query = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &Health,
        &UnitStats,
    )>();
    query
        .iter(world)
        .map(|(entity, faction, position, health, stats)| {
            (
                entity.to_bits(),
                UnitTraceSnapshot {
                    player_id: faction.0.0,
                    unit_type: stats.unit_type,
                    health: health.current,
                    max_health: health.max,
                    cost: stats.cost,
                    can_capture: stats.can_capture,
                    position: *position,
                },
            )
        })
        .collect()
}
```

Use Japanese comments to explain why a missing post-action entity means destruction rather than missing data.

- [ ] **Step 4: Run the pure tests and verify pass**

Run:

```text
cargo test -p mcp-server invasion_trace::tests
```

Expected: 3 tests pass.

- [ ] **Step 5: Extend `InvasionEvent` with structured production and combat fields**

Add:

```rust
UnitProduced {
    turn: u32,
    player_id: u32,
    step: usize,
    unit_id: u64,
    unit_type: String,
    cost: u32,
    max_cargo: u32,
    can_capture: bool,
    x: usize,
    y: usize,
},
```

Replace the existing `UnitAttacked` fields with:

```rust
UnitAttacked {
    turn: u32,
    player_id: u32,
    step: usize,
    attacker_id: u64,
    attacker_player_id: u32,
    attacker_unit_type: String,
    defender_id: u64,
    defender_player_id: u32,
    defender_unit_type: String,
    damage_value_dealt: i64,
    counter_value_received: i64,
},
```

Add `produced_cursor: EventCursor<UnitProducedEvent>` to `InvasionTraceCollector` and initialize it in `new`.

- [ ] **Step 6: Change `collect_step` to consume before/after snapshots**

Change the signature to:

```rust
pub fn collect_step(
    &mut self,
    world: &mut World,
    turn: u32,
    player_id: u32,
    step: usize,
    units_before: &HashMap<u64, UnitTraceSnapshot>,
) -> Vec<InvasionEvent>
```

At the start of the method, build `let units_after = snapshot_units(world);`. For each `UnitAttackedEvent`, retrieve attacker and defender from `units_before`; if either before-snapshot is missing, skip the malformed diagnostic event rather than panic. Calculate:

```rust
let damage_value_dealt = calculate_damage_value(
    defender_before,
    units_after.get(&event.defender.to_bits()),
);
let counter_value_received = calculate_damage_value(
    attacker_before,
    units_after.get(&event.attacker.to_bits()),
);
```

For each `UnitProducedEvent`, read the produced entity from `units_after` and emit the structured event. Keep Load/Unload/Capture behavior unchanged, replacing `positions_before` lookups with `units_before.get(...).map(|unit| unit.position)`.

- [ ] **Step 7: Update MCP call sites and board-state unit fields**

In `mcp-server/src/main.rs`, replace:

```rust
let positions_before = invasion_trace::snapshot_unit_positions(&mut state.world);
```

with:

```rust
let units_before = invasion_trace::snapshot_units(&mut state.world);
```

Pass `&units_before` to `collect_step`, and pass `&mut state.world` because snapshot collection needs a mutable query.

In the board-state unit JSON add:

```rust
"cost": stats.cost,
"can_capture": stats.can_capture,
"max_cargo": stats.max_cargo,
"loadable_unit_types": stats
    .loadable_unit_types
    .iter()
    .map(|unit_type| unit_type.as_str())
    .collect::<Vec<_>>(),
```

These fields are diagnostic facts, not game rules. `UnitProduced` must copy `max_cargo` and `can_capture` from the produced unit snapshot so Python can distinguish transport capacity, capture supply, and combat investment without parsing debug strings.

- [ ] **Step 8: Run MCP and workspace tests**

Run:

```text
cargo test -p mcp-server
cargo test
```

Expected: all tests pass.

- [ ] **Step 9: Run formatting and clippy for the changed Rust code**

Run:

```text
cargo fmt --all -- --check
cargo clippy -p mcp-server --all-targets --all-features -- -D warnings
```

Expected: both commands pass.

- [ ] **Step 10: Inspect the checkpoint without committing**

Run:

```text
git diff --check -- mcp-server/src/invasion_trace.rs mcp-server/src/main.rs
git diff --stat -- mcp-server/src/invasion_trace.rs mcp-server/src/main.rs
```

Expected: no whitespace errors and only the intended MCP files are listed.

---

### Task 4: Analyze per-game occupation, investment, ROI, and milestones

**Files:**
- Modify: `scripts/eval_issue58.py`
- Modify: `scripts/test_eval_issue58.py`

**Interfaces:**
- Consumes: one game result from `run_single_game`, including `initial_state`, `final_state`, `metrics`, `strategic_history`, and structured `invasion_events`.
- Produces:
  - `subject_won(game: dict, subject_player: int) -> bool`
  - `analyze_issue58_game(game: dict, subject: str = "V3", baseline: str = "V2") -> dict | None`
  - `production_investment(events: list[dict], player_id: int) -> dict[str, int]`
  - `combat_value_by_unit_type(events: list[dict], player_id: int) -> dict[str, int]`
  - `first_event_turn(events: list[dict], event_type: str, player_id: int) -> int | None`

- [ ] **Step 1: Write failing analysis fixture tests**

Add helpers and tests to `scripts/test_eval_issue58.py`:

```python
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
                {"x": 0, "y": 0, "terrain": "Capital", "owner": 1, "island_id": 0},
                {"x": 9, "y": 9, "terrain": "Capital", "owner": 2, "island_id": 5},
            ]
        },
        "final_state": {
            "properties": [
                {"x": 0, "y": 0, "terrain": "Capital", "owner": 1, "island_id": 0},
                {"x": 9, "y": 9, "terrain": "Capital", "owner": 2, "island_id": 5},
                {"x": 1, "y": 0, "terrain": "City", "owner": 2, "island_id": 0},
            ],
            "units": [
                {"unit_id": 20, "player_id": 2, "unit_type": "Infantry",
                 "can_capture": True, "hp": 70, "x": 1, "y": 0, "island_id": 0},
            ],
        },
        "metrics": [{
            "turn": 30,
            "p1_units": 10_000,
            "p2_units": 12_000,
            "p1_obj": {"zoc_area": 10, "income_per_turn": 4_000, "owned_properties": 1},
            "p2_obj": {"zoc_area": 12, "income_per_turn": 5_000, "owned_properties": 2},
        }],
        "invasion_events": [
            {"type": "unit_produced", "turn": 1, "player_id": 2, "unit_id": 30,
             "unit_type": "Lander", "cost": 12_000, "max_cargo": 2,
             "can_capture": False, "x": 9, "y": 8},
            {"type": "unit_produced", "turn": 2, "player_id": 2, "unit_id": 40,
             "unit_type": "Battleship", "cost": 28_000, "max_cargo": 0,
             "can_capture": False, "x": 9, "y": 8},
            {"type": "unit_loaded", "turn": 3, "player_id": 2, "transport_id": 10,
             "cargo_id": 20, "island_id": 5},
            {"type": "unit_unloaded", "turn": 6, "player_id": 2, "transport_id": 10,
             "cargo_id": 20, "unit_type": "Infantry", "can_capture": True,
             "island_id": 0, "x": 1, "y": 0},
            {"type": "property_capture_progressed", "turn": 7, "player_id": 2,
             "unit_id": 20, "island_id": 0, "x": 1, "y": 0,
             "completed": False},
            {"type": "property_capture_progressed", "turn": 8, "player_id": 2,
             "unit_id": 20, "island_id": 0, "x": 1, "y": 0,
             "completed": True},
            {"type": "unit_attacked", "turn": 9, "player_id": 2,
             "attacker_player_id": 2, "attacker_unit_type": "Battleship",
             "defender_player_id": 1, "defender_unit_type": "Infantry",
             "damage_value_dealt": 500, "counter_value_received": 0},
        ],
        "strategic_history": [
            {"turn": 6, "properties": [], "units": [
                {"unit_id": 20, "player_id": 2, "unit_type": "Infantry",
                 "can_capture": True, "hp": 100, "x": 1, "y": 0, "island_id": 0},
            ]},
            {"turn": 8, "properties": [
                {"x": 1, "y": 0, "terrain": "City", "owner": 2, "island_id": 0},
            ], "units": [
                {"unit_id": 20, "player_id": 2, "unit_type": "Infantry",
                 "can_capture": True, "hp": 70, "x": 1, "y": 0, "island_id": 0},
            ]},
        ],
    }


class Issue58GameAnalysisTests(unittest.TestCase):
    def test_detects_external_property_gain_and_capture_throughput(self):
        from scripts.eval_issue58 import analyze_issue58_game
        analysis = analyze_issue58_game(make_issue58_game())
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

    def test_calculates_battleship_investment_and_damage_roi(self):
        from scripts.eval_issue58 import analyze_issue58_game
        analysis = analyze_issue58_game(make_issue58_game())
        self.assertEqual(28_000, analysis["production_investment"]["Battleship"])
        self.assertEqual(500, analysis["combat_value_by_unit_type"]["Battleship"])
        self.assertAlmostEqual(500 / 28_000, analysis["battleship_roi"])
```

- [ ] **Step 2: Run the game-analysis tests and verify failure**

Run:

```text
python -m unittest scripts.test_eval_issue58.Issue58GameAnalysisTests
```

Expected: import or attribute failure for the analysis functions.

- [ ] **Step 3: Implement deterministic per-game analysis**

Implement helpers that:

1. Determine the subject player, baseline player, subject side, baseline side, and order from `p1`/`p2`.
2. Define `subject_won(game, subject_player)` so `P1_Win*` and `P2_Win*` results map to the correct player and draws return `False`.
3. Read both subject and baseline objective metrics from the final completed-round metric.
4. Compute initial subject island IDs from initially owned properties.
5. Count final subject-owned properties whose `island_id` is outside that initial set.
6. Track every external property first acquired in `strategic_history`, then classify it as retained or lost by the final state.
7. Deduplicate capture starts/completions by `(unit_id, x, y)`.
8. Link the first unload of a capture-capable cargo to its first capture progress and determine whether each landed capture unit survives through capture completion or the final state.
9. Sum production cost by `unit_type` from `unit_produced` events, sum `max_cargo` as produced transport capacity, and record the first production turn with `max_cargo > 0`.
10. Sum `damage_value_dealt` for attacks where `attacker_player_id` is the subject.
11. From square-grid `strategic_history`, calculate the minimum Manhattan distance from a surviving subject capture unit on the target island to the enemy capital each completed round; record the first and minimum distances so capital pressure can be evaluated without hard-coded coordinates.
12. Select subject thinking times from player 1 or player 2 according to the subject side.
13. Return `None` when the requested subject is not in the game.

Use this result shape:

```python
return {
    "map": game.get("map"),
    "seed": game.get("seed"),
    "order": order,
    "subject_player": subject_player,
    "error": game.get("error"),
    "result": game.get("result"),
    "won": subject_won(game, subject_player),
    "final_zoc": subject_objective.get("zoc_area", 0),
    "final_income": subject_objective.get("income_per_turn", 0),
    "final_properties": subject_objective.get("owned_properties", 0),
    "baseline_final_zoc": baseline_objective.get("zoc_area", 0),
    "baseline_final_income": baseline_objective.get("income_per_turn", 0),
    "baseline_final_properties": baseline_objective.get("owned_properties", 0),
    "external_properties_gained": external_properties_gained,
    "external_properties_retained": external_properties_retained,
    "external_properties_lost_after_capture": external_properties_lost_after_capture,
    "landed_capture_units": len(landed_capture_ids),
    "capture_started": len(capture_started),
    "capture_completed": len(capture_completed),
    "capture_unit_survival_rate": capture_unit_survival_rate,
    "landing_to_capture_turns": landing_to_capture_turns,
    "capital_distance_first": capital_distance_first,
    "capital_distance_min": capital_distance_min,
    "milestones": milestones,
    "transport_capacity_produced": transport_capacity_produced,
    "production_investment": dict(sorted(investment.items())),
    "combat_value_by_unit_type": dict(sorted(combat_value.items())),
    "battleship_roi": battleship_damage / battleship_investment
        if battleship_investment else None,
    "asset_trend_ok": check_no_decline(asset_series),
    "income_trend_ok": check_no_decline(income_series),
    "thinking_ms": thinking_times,
}
```

Import or move `check_no_decline` so there is one shared implementation. Do not duplicate its formula.

- [ ] **Step 4: Enrich unload traces with unit role facts**

In `mcp-server/src/invasion_trace.rs`, extend `UnitUnloaded` with:

```rust
unit_type: String,
can_capture: bool,
```

Read these values from the post-action unit snapshot for the cargo. Update existing Issue #54 Python tests to tolerate the extra fields; do not change their pass/fail semantics.

- [ ] **Step 5: Run Python and MCP focused tests**

Run:

```text
python -m unittest scripts.test_eval_issue58.Issue58GameAnalysisTests
python -m unittest scripts.test_eval_matchup
cargo test -p mcp-server
```

Expected: all pass.

- [ ] **Step 6: Inspect the checkpoint without committing**

Run:

```text
git diff --check -- scripts/eval_issue58.py scripts/test_eval_issue58.py mcp-server/src/invasion_trace.rs
git diff --stat -- scripts/eval_issue58.py scripts/test_eval_issue58.py mcp-server/src/invasion_trace.rs
```

Expected: no whitespace errors.

---

### Task 5: Implement Issue #58 aggregation and acceptance criteria

**Files:**
- Modify: `scripts/eval_issue58.py`
- Modify: `scripts/test_eval_issue58.py`

**Interfaces:**
- Consumes: `analyze_issue58_game` results for V3 and the corresponding V2 opponent metrics.
- Produces:
  - `judge_issue58_criteria(results: list[dict], subject: str = "V3", baseline: str = "V2") -> tuple[bool, list[dict], list[dict]]`
  - `average(values: list[float]) -> float`
  - `percentile95(values: list[float]) -> float`

- [ ] **Step 1: Write failing equal-income and strict-ZOC tests**

Add:

```python
from scripts.eval_issue58 import judge_issue58_criteria


def metric_game(order, seed, subject_zoc, baseline_zoc, subject_income, baseline_income):
    p1, p2 = ("V3", "V2") if order == "先攻" else ("V2", "V3")
    subject_side = "p1" if p1 == "V3" else "p2"
    baseline_side = "p2" if subject_side == "p1" else "p1"
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
        "map": "map_3", "p1": p1, "p2": p2, "seed": seed,
        "result": "P1_Win_MaxTurns", "error": None,
        "initial_state": {"properties": [
            {"owner": 1, "island_id": 0}, {"owner": 2, "island_id": 5},
        ]},
        "final_state": {"properties": [
            {"owner": 1, "island_id": 0}, {"owner": 2, "island_id": 5},
            {"owner": 1 if subject_side == "p1" else 2, "island_id": 5 if subject_side == "p1" else 0},
        ]},
        "metrics": [metric] * 15,
        "invasion_events": [], "strategic_history": [],
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
        games = [metric_game("先攻", seed, 11, 10, 5000, 5000) for seed in (1, 2, 3, 4)]
        games += [metric_game("後攻", seed, 11, 10, 5000, 5000) for seed in (1, 2, 3)]
        games[0]["error"] = "MCP failed"
        overall, rows, _ = judge_issue58_criteria(games)
        self.assertFalse(overall)
        self.assertTrue(any(row["errors"] for row in rows))
        self.assertTrue(any(row["games"] < 4 for row in rows))
```

- [ ] **Step 2: Run criteria tests and verify failure**

Run:

```text
python -m unittest scripts.test_eval_issue58.Issue58CriteriaTests
```

Expected: import or attribute failure.

- [ ] **Step 3: Implement hand-order aggregation**

Bucket by `(map, order)` and keep subject/baseline values from the same games. For each bucket calculate:

```python
subject_zoc = [analysis["final_zoc"] for analysis in bucket]
baseline_zoc = [analysis["baseline_final_zoc"] for analysis in bucket]
subject_income = [analysis["final_income"] for analysis in bucket]
baseline_income = [analysis["baseline_final_income"] for analysis in bucket]
subject_properties = [analysis["final_properties"] for analysis in bucket]
baseline_properties = [analysis["baseline_final_properties"] for analysis in bucket]

zoc_pass = average(subject_zoc) > average(baseline_zoc)
income_pass = average(subject_income) >= average(baseline_income)
trend_pass = all(
    analysis["asset_trend_ok"] and analysis["income_trend_ok"]
    for analysis in bucket
)
external_property_pass = (
    map_name != "map_3"
    or order != "後攻"
    or any(analysis["external_properties_gained"] > 0 for analysis in bucket)
)
second_player_properties_pass = (
    map_name != "map_3"
    or order != "後攻"
    or average(subject_properties) >= average(baseline_properties)
)
complete = len(bucket) >= 4 and len({analysis["seed"] for analysis in bucket}) >= 4 \
    and not any(analysis["error"] for analysis in bucket)
passed = (
    complete
    and zoc_pass
    and income_pass
    and trend_pass
    and external_property_pass
    and second_player_properties_pass
)
```

Record map_1 and map_2 rows using the same objective criteria so the later result run can compare baseline PASS buckets, but exclude those maps from the Issue #58 overall verdict. Compute overall only from both map_3 order rows plus the map_3 win-rate guardrail. Do not count errored games as wins or silently remove them.

- [ ] **Step 4: Add thinking-time summary helpers**

Implement average, median using `statistics.median`, and a deterministic nearest-rank 95th percentile:

```python
def percentile95(values):
    if not values:
        return 0.0
    ordered = sorted(values)
    index = max(0, math.ceil(len(ordered) * 0.95) - 1)
    return ordered[index]
```

The 50% regression gate is applied later by comparing baseline and result JSON, not while producing a baseline alone.

- [ ] **Step 5: Run criteria tests and verify pass**

Run:

```text
python -m unittest scripts.test_eval_issue58.Issue58CriteriaTests
```

Expected: pass.

- [ ] **Step 6: Run the complete Python test set**

Run:

```text
python -m unittest scripts.test_eval_matchup scripts.test_eval_issue58
```

Expected: all tests pass.

- [ ] **Step 7: Inspect the checkpoint without committing**

Run:

```text
git diff --check -- scripts/eval_issue58.py scripts/test_eval_issue58.py
git diff --stat -- scripts/eval_issue58.py scripts/test_eval_issue58.py
```

Expected: no whitespace errors.

---

### Task 6: Integrate safe JSON/Markdown artifact generation into the CLI

**Files:**
- Modify: `scripts/eval_matchup.py:671-792`
- Modify: `scripts/eval_issue58.py`
- Modify: `scripts/test_eval_issue58.py`
- Create: `benchmarks/issue-58/seeds.txt`

**Interfaces:**
- Consumes: Task 1 metadata, Task 2 game results, Task 5 criteria rows.
- Produces:
  - CLI options `--criteria issue58`, `--seeds`, and `--json-output`.
  - `write_json_atomic(path: str, payload: dict) -> None`
  - `write_text_atomic(path: str, content: str) -> None`
  - `generate_issue58_report(payload: dict) -> str`

- [ ] **Step 1: Write failing atomic-output and report metadata tests**

Add:

```python
from scripts.eval_issue58 import generate_issue58_report, write_json_atomic, write_text_atomic


class Issue58ArtifactTests(unittest.TestCase):
    def test_report_contains_reproducibility_metadata(self):
        report = generate_issue58_report({
            "metadata": {
                "commit_sha": "abc123",
                "working_tree_dirty": True,
                "command": ["python", "scripts/eval_matchup.py"],
                "seeds": [1, 2, 3, 4],
                "games_per_order": 4,
                "evaluator_sha256": "e" * 64,
                "mcp_sha256": "m" * 64,
            },
            "overall_pass": False,
            "criteria_rows": [],
            "analyses": [],
            "results": [],
        })
        self.assertIn("abc123", report)
        self.assertIn("working tree: dirty", report)
        self.assertIn("1, 2, 3, 4", report)
        self.assertIn("4", report)

    def test_atomic_writers_create_parent_and_final_file(self):
        with TemporaryDirectory() as directory:
            text_path = Path(directory) / "nested" / "report.md"
            json_path = Path(directory) / "nested" / "report.json"
            write_text_atomic(str(text_path), "report")
            write_json_atomic(str(json_path), {"ok": True})
            self.assertEqual("report", text_path.read_text(encoding="utf-8"))
            self.assertEqual({"ok": True}, json.loads(json_path.read_text(encoding="utf-8")))
            self.assertFalse(text_path.with_suffix(".md.tmp").exists())
```

- [ ] **Step 2: Run artifact tests and verify failure**

Run:

```text
python -m unittest scripts.test_eval_issue58.Issue58ArtifactTests
```

Expected: missing-function failure.

- [ ] **Step 3: Implement atomic writers**

Use a sibling temporary file and `os.replace`:

```python
def write_text_atomic(path, content):
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary = target.with_name(f"{target.name}.tmp")
    temporary.write_text(content, encoding="utf-8")
    os.replace(temporary, target)


def write_json_atomic(path, payload):
    write_text_atomic(path, json.dumps(payload, ensure_ascii=False, indent=2) + "\n")
```

- [ ] **Step 4: Implement the Markdown report sections**

`generate_issue58_report` must render these sections from JSON data only:

1. Reproducibility metadata.
2. Overall PASS/FAIL.
3. Map/order criteria table.
4. Win rate and thinking-time summary.
5. Occupation throughput by seed/order.
6. Production investment by unit type.
7. Battleship investment, damage value, and ROI by game.
8. First production/Load/Drop/combat/capture milestones.
9. Error list.

Do not reread live game state while rendering.

- [ ] **Step 5: Add CLI arguments and Issue #58 dispatch**

In `main()` add:

```python
parser.add_argument("--criteria", choices=["objective", "issue54", "issue58"], default="objective")
parser.add_argument("--seeds", default=None, help="Comma-separated deterministic seed set")
parser.add_argument("--json-output", default=None, help="Raw JSON output path")
```

For `issue58`:

1. Parse `--seeds` with `parse_seed_list`; reject missing `--seeds` even if legacy `--seed` is present.
2. Require `--json-output`.
3. Call `validate_issue58_run` before starting the MCP process.
4. Build specs with `build_match_specs`.
5. Add `map`, `p1`, `p2`, and `seed` to every result.
6. Build the payload with metadata, results, analyses, criteria rows, and overall status.
7. Write JSON first and Markdown second with atomic writers.
8. Exit 1 when criteria fail and exit 2 when execution errors prevent a complete evaluation.

Keep objective and Issue #54 behavior backward-compatible.

- [ ] **Step 6: Create the canonical seed file**

Create `benchmarks/issue-58/seeds.txt`:

```text
58001
58002
58003
58004
```

- [ ] **Step 7: Run focused and complete Python tests**

Run:

```text
python -m unittest scripts.test_eval_issue58.Issue58ArtifactTests
python -m unittest scripts.test_eval_matchup scripts.test_eval_issue58
```

Expected: all pass.

- [ ] **Step 8: Verify CLI validation without running games**

Run:

```text
python scripts/eval_matchup.py --mode batch --map map_3 --p1 V3 --p2 V2 --criteria issue58 --seeds 1,2,3 --max-turns 30 --output benchmarks/issue-58/invalid.md --json-output benchmarks/issue-58/invalid.json
```

Expected: non-zero exit before MCP startup with a message containing `at least 4` and no output files created.

Run:

```text
python scripts/eval_matchup.py --mode batch --map map_3 --p1 V3 --p2 V2 --criteria issue58 --seeds 1,2,3,4 --max-turns 30 --output matchup_report.md --json-output benchmarks/issue-58/invalid.json
```

Expected: non-zero exit before MCP startup with a message naming `matchup_report.md`.

- [ ] **Step 9: Inspect the checkpoint without committing**

Run:

```text
git diff --check -- scripts benchmarks/issue-58/seeds.txt
git diff --stat -- scripts benchmarks/issue-58/seeds.txt
```

Expected: no whitespace errors.

---

### Task 7: Run all quality gates before baseline

**Files:**
- No new files unless formatting tools modify intended source files.

**Interfaces:**
- Consumes: Tasks 1-6.
- Produces: a verified evaluator and telemetry build suitable for baseline execution.

- [ ] **Step 1: Run all Python tests**

Run:

```text
python -m unittest scripts.test_eval_matchup scripts.test_eval_issue58
```

Expected: all tests pass.

- [ ] **Step 2: Run the Rust workspace tests**

Run:

```text
cargo test
```

Expected: all tests pass.

- [ ] **Step 3: Run workspace clippy**

Run:

```text
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: no warnings or errors.

- [ ] **Step 4: Run formatting verification**

Run:

```text
cargo fmt --all -- --check
```

Expected: no formatting differences.

- [ ] **Step 5: Build the release MCP server used by the evaluator**

Run:

```text
cargo build --release -p mcp-server
```

Expected: `target/release/mcp-server.exe` exists on Windows and the build succeeds.

- [ ] **Step 6: Inspect repository status without committing**

Run:

```text
git status --short
git diff --check
```

Expected: only intended source, spec/plan, and seed files are changed; no whitespace errors.

---

### Task 8: Execute the fixed-seed baseline and write evidence artifacts

**Files:**
- Create: `benchmarks/issue-58/baseline-$BASE_SHA.md`
- Create: `benchmarks/issue-58/baseline-$BASE_SHA.json`
- Create: `benchmarks/issue-58/analysis.md`
- Create: `benchmarks/issue-58/child-issue-drafts.md`

**Interfaces:**
- Consumes: release MCP server and Issue #58 evaluator.
- Produces: reproducible baseline, hypothesis classification, and local child-issue drafts.

- [ ] **Step 1: Capture the AI source commit SHA and run the complete baseline**

Run as one PowerShell command so the filename and recorded SHA cannot diverge:

```text
$baseSha = (git rev-parse --short=7 HEAD).Trim(); python scripts/eval_matchup.py --mode batch --map map_1,map_2,map_3 --p1 V3 --p2 V2 --criteria issue58 --seeds 58001,58002,58003,58004 --max-turns 30 --output "benchmarks/issue-58/baseline-$baseSha.md" --json-output "benchmarks/issue-58/baseline-$baseSha.json"
```

Expected:

- 24 total games: 3 maps × 4 seeds × 2 orders.
- The process may exit 1 because baseline criteria are expected to expose failures.
- It must not exit 2.
- Both baseline files must exist and contain 24 results.
- `matchup_report.md` must remain unchanged.

- [ ] **Step 2: Verify baseline reproducibility metadata**

Run:

```text
$baseSha = (git rev-parse --short=7 HEAD).Trim(); $env:BASELINE_JSON = "benchmarks/issue-58/baseline-$baseSha.json"; $env:BASE_SHA = $baseSha; python -c "import json, os; p=json.load(open(os.environ['BASELINE_JSON'], encoding='utf-8')); assert len(p['results']) == 24; assert p['metadata']['seeds'] == [58001,58002,58003,58004]; assert p['metadata']['games_per_order'] == 4; assert p['metadata']['commit_sha'].startswith(os.environ['BASE_SHA']); assert not any(g.get('error') for g in p['results'])"
```

Expected: exit 0.

- [ ] **Step 3: Check deterministic repeatability on one seed**

Run the same one-seed game twice outside Issue #58 criteria using the legacy objective path and separate temporary outputs, or call `run_single_game` twice from a short Python command. Compare these normalized fields:

```text
result
metrics
final_state.properties
final_state.units
invasion_events
```

Expected: exact equality for seed `58001`, map_3, P1=V3, P2=V2. Do not compare wall-clock `thinking_times`.

- [ ] **Step 4: Write `analysis.md` from the baseline evidence**

Create these sections in order:

1. `# Issue #58 Baseline Analysis`.
2. `## Reproducibility`, populated directly from `payload["metadata"]`: full commit SHA, evaluator SHA-256, MCP SHA-256, the four seeds, 4 games per order per map, 24 total games, and the exact command array joined with spaces.
3. `## Acceptance Baseline`, reproducing every generated criteria row with map, order, subject/baseline ZOC, income, properties, trend result, external-property result, completeness, and PASS/FAIL.
4. `## Hypothesis Decisions`, containing exactly six rows: capture-unit shortage, post-landing dispersion or delay, Battleship overinvestment, escort shortage, missing capital pressure, and second-player startup delay. Each row must contain one final decision and numeric seed/order evidence.
5. `## Confirmed Root Causes`, with one subsection per confirmed cause naming affected map/order/seeds and exact observed values.
6. `## Rejected Hypotheses`, with one subsection per rejected hypothesis and the numeric evidence that contradicts it.
7. `## Inconclusive Hypotheses`, with one subsection per inconclusive hypothesis and the exact telemetry field or comparison that is missing.
8. `## Required AI-Fix Plan Inputs`, listing the exact affected modules inferred from traces, one deterministic regression scenario per confirmed cause, and the baseline values the result run must improve.

Classification rules:

- `confirmed`: evidence shows the hypothesized behavior in at least two fixed-seed map_3 games and it co-occurs with a failed acceptance bucket or lost/stalled invasion.
- `rejected`: all relevant fixed-seed map_3 games show the opposite behavior.
- `inconclusive`: neither condition is met or required telemetry is absent.

Do not label a hypothesis confirmed from a single anecdotal game.

- [ ] **Step 5: Write one local child-issue draft per confirmed cause**

For every confirmed cause, add one section to `benchmarks/issue-58/child-issue-drafts.md` with all of these concrete fields:

1. A title beginning `[AI V3]` that names one cause and one intended outcome.
2. Parent `Issue #58`.
3. Affected map, order, and explicit seed numbers from the baseline JSON.
4. The exact failing baseline metric values.
5. The relevant milestone, investment, ROI, survival, or target-history trace values.
6. A scope statement containing one cause and one measurable behavior change.
7. The exact engine file or files implicated by the trace.
8. Acceptance checkboxes requiring a deterministic failing test, no map/order/seed-specific production logic, improvement of the named fixed-seed metric, no regression of map_1/map_2 baseline PASS buckets, and successful Rust quality gates.

Do not create a draft for rejected or inconclusive hypotheses, and do not leave field labels without values.

- [ ] **Step 6: Validate analysis completeness**

Check that every hypothesis appears exactly once in `confirmed`, `rejected`, or `inconclusive`, every confirmed cause has one child draft, and every child draft names numeric evidence from the baseline JSON.

Run:

```text
git diff --check -- benchmarks/issue-58
```

Expected: no whitespace errors.

- [ ] **Step 7: Inspect final Phase 1 status without committing**

Run:

```text
git status --short
git diff --stat
```

Expected: source changes, tests, design/plan documents, seeds, baseline artifacts, analysis, and local child drafts are present; no unrelated tracked file is modified.

---

### Task 9: Create the cause-specific AI implementation plan

**Files:**
- Read: `benchmarks/issue-58/analysis.md`
- Read: `benchmarks/issue-58/child-issue-drafts.md`
- Read: `benchmarks/issue-58/baseline-$BASE_SHA.json`
- Create: `docs/superpowers/plans/2026-07-27-issue-58-ai-fixes.md`

**Interfaces:**
- Consumes: only causes marked `confirmed` in Task 8.
- Produces: a second executable TDD plan that names exact `engine/src/ai/*.rs` changes, deterministic tests, intermediate fixed-seed checks, full result evaluation, 50% thinking-time gate, and map_1/map_2 regression comparison.

- [ ] **Step 1: Map each confirmed cause to one deterministic failing test**

For each confirmed cause, identify the smallest existing test module capable of reproducing it:

- post-landing assignment/dispersion → `engine/src/ai/island_invasion_tests.rs` or `engine/src/ai/squad.rs` tests
- target ordering/capital pressure → `engine/src/ai/objectives.rs` or `engine/src/ai/strategy.rs` tests
- production composition/high-cost investment → `engine/src/ai/production.rs` tests
- beam-search target loss → `engine/src/ai/beam_search.rs` tests

Use actual trace entities, unit roles, and state transitions from baseline to define inputs. Do not include any rejected or inconclusive cause.

- [ ] **Step 2: Write the second plan with the required header and task structure**

The second plan must include:

1. One TDD task per confirmed cause.
2. A short fixed-seed map_3 verification after each cause.
3. A full result run using the same 24-game matrix.
4. `compare_issue58_runs` coverage for:
   - map_3 acceptance criteria,
   - map_1/map_2 PASS-bucket regression,
   - win rate ≥40%,
   - mean map_3 thinking time ≤ baseline × 1.50.
5. Final `cargo test`, clippy, fmt, Python tests, and artifact verification.
6. `benchmarks/issue-58/result-$RESULT_SHA.md` and `benchmarks/issue-58/result-$RESULT_SHA.json` and updated `analysis.md` mapping fixes to outcomes.

- [ ] **Step 3: Self-review the second plan against baseline evidence**

Verify:

- every confirmed cause has exactly one implementation task;
- no task implements an unconfirmed hypothesis;
- every test has exact setup and assertions;
- every function/type used by a later task is defined earlier;
- no map_3 coordinate, player order, or seed appears in production AI logic;
- no placeholder language remains.

- [ ] **Step 4: Present the second plan for execution choice**

Offer inline execution or user-requested subagent execution. Do not start AI modifications before the user approves the second plan.

---

## Phase 1 Completion Criteria

This plan is complete when:

- fixed-seed paired-order execution exists;
- T30 is measured after both players finish;
- Issue #58 income equality passes and ZOC equality fails;
- structured production, combat, occupation, and milestone telemetry exists;
- raw JSON and Markdown are written atomically without touching `matchup_report.md`;
- all Python/Rust tests, clippy, and fmt pass;
- the release MCP server builds;
- the 24-game baseline is saved with reproducibility metadata;
- all six hypotheses are classified with numeric evidence;
- one local child draft exists per confirmed cause;
- a separate, evidence-specific AI-fix plan is written and approved before engine strategy changes begin.
