# Issue #58 Island Campaign Portfolio Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** V3 AIを単一の敵島侵攻目標から全島を毎ターン評価する島嶼キャンペーン・ポートフォリオへ移行し、中立島への低コスト拡張、最大3島の並行作戦、島別の継続・増援・撤退、防衛優先、予算を満たした敵島侵攻を決定論的に実行できるようにする。

**Architecture:** `engine` に純粋な島状態・判断・予算・割当ルールと、ECS盤面を評価入力へ変換する分析層を追加する。`ProductionStrategy`、`SquadManager`、生産選択は毎ターン再構築された `IslandCampaignPortfolio` だけを参照し、進行中作戦は既存Squadから復元する。MCPは判断に使わない診断スナップショットを公開し、Python評価基盤はV3対V1の24試合とV3自己対戦の4試合を別プロトコルとして保存・判定する。

**Tech Stack:** Rust 2024、bevy_ecs 0.15.2、serde、rmcp、Python 3標準ライブラリ（argparse/hashlib/json/statistics/subprocess/unittest）、cargo workspace、GitHub Flow。

## Global Constraints

- Do not commit unless the user explicitly authorizes it.
- Do not create GitHub child issues; write local drafts to `benchmarks/issue-58/child-issue-drafts.md`.
- Never overwrite `matchup_report.md` in Issue #58 mode.
- パッケージ管理には必ず `pnpm` を使用してください。`npm` や `yarn` の使用は厳禁です。
- ソースコードにはロジックの内容がわかるように日本語のコメントをいれること。
- Do not add map_3 coordinates, player-order branches, or seed-specific AI behavior to production AI logic.
- Do not modify V1 transport behavior or reimplement Load/Transit/Drop rules.
- Domain rules must live in `engine`, never in presentation layers (`cli` / `gui`).
- 全島評価は盤面から毎ターン再構築し、新しい永続的な作戦ライフサイクル状態を追加しない。進行中作戦は既存の `SquadManager` から復元する。
- 新規作戦は完全編成を確保できる場合だけ開始し、同じ資金・輸送・占領・戦闘ユニットを複数島へ二重割当しない。
- 攻勢作戦は `Expand` / `Contest` / `Reinforce` / `Assault` を合計して最大3島とする。`Secured` の管理と `Threatened` の防衛は上限に含めない。
- OpenNeutralの原則編成は輸送ヘリ1体（4,000G）＋占領可能ユニット2体（軽歩兵なら2,000G）、合計6,000Gとする。
- EnemyHeldの最低侵攻予算は32,700Gとし、`required_combat_budget = max(10,200G, ceil(enemy_combat_value * 1.2))`、`required_assault_budget = 22,500G + required_combat_budget` とする。
- 装甲車の `max_cargo` は海を越える輸送需要へ計上せず、地上輸送需要だけを満たす。
- 比較評価はmap_1/map_2/map_3、seeds 58001/58002/58003/58004、V3=P1/P2の両順序、30ターン、合計24試合とする。
- 自己対戦評価はmap_3、V3対V3、同じ4 seedを各1試合、30ターン、合計4試合とし、両プレイヤーを別々に判定する。
- Quality gates: `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, `cargo test` must all pass.

---

## File Structure

### 新規ファイル

- `engine/src/ai/island_campaign.rs`
  - 状態・判断・評価・割当型、状態分類、ROI、侵攻予算、Contested判断、決定論的な資源予約と最大3作戦割当を実装する純粋ドメインモジュール。
- `engine/src/ai/island_campaign_analysis.rs`
  - ECS盤面、`IslandMap`、`SquadManager`、マスターデータ、移動距離から島別入力を構築し、`IslandCampaignPortfolio` を返す分析モジュール。
- `engine/src/ai/island_campaign_tests.rs`
  - 複数島、脅威優先、撤退、予算、spawn順のECS統合テストを集約する。

### 変更ファイル

- `engine/src/ai/mod.rs`
  - 新規モジュールを登録し、統合テストモジュールを有効化する。
- `engine/src/ai/objectives.rs`
  - `InvasionTarget` を削除し、既存 `Objective` は島内占領対象のスコアリング用途だけに残す。
- `engine/src/ai/strategy.rs`
  - `ProductionStrategy::invasion_target` を `campaign_portfolio` へ置換し、ポートフォリオの不足編成を生産需要へ集約する。
- `engine/src/ai/squad.rs`
  - 単一島フィルタを廃止し、島別Assignmentに従って既存の輸送・占領・戦闘Squadを維持または追加する。
- `engine/src/ai/production.rs`
  - ポートフォリオの島別対象と不足編成を消費し、海上輸送需要から装甲車を除外し、同点時の選択を決定論的にする。
- `engine/src/ai/island_invasion_tests.rs`
  - 既存の単一敵島期待値をポートフォリオ期待値へ移行し、既存Load/Transit/Drop回帰を保持する。
- `mcp-server/src/invasion_trace.rs`
  - ターンごとの島状態、判断理由、ETA、予算、割当Entity IDを直列化する診断DTOを追加する。
- `mcp-server/src/main.rs`
  - `simulate_ai_turn` 応答へ `island_campaign` を追加する。
- `scripts/eval_matchup.py`
  - Issue #58の比較・自己対戦スケジュール、baseline/result段階、島テレメトリ収集を追加する。
- `scripts/eval_issue58.py`
  - 2プロトコルの検証、自己対戦の両側分析、ポートフォリオ受け入れ条件、baseline比較を追加する。
- `scripts/test_eval_matchup.py`
  - 24試合と4試合のスケジュール、CLI分岐、島履歴の収集をテストする。
- `scripts/test_eval_issue58.py`
  - プロトコル検証、両側分析、状態欠損・二重割当・予算違反のFAIL、baseline/result比較をテストする。
- `benchmarks/issue-58/analysis.md`
  - 実装前後の2プロトコル結果、ガードレール、残課題を記録する。

### 生成する評価成果物

- `benchmarks/issue-58/baseline-v3-v1-<SHA>.json` — 実装前V3対V1、24試合。
- `benchmarks/issue-58/baseline-v3-selfplay-<SHA>.json` — 実装前V3自己対戦、4試合。
- `benchmarks/issue-58/result-v3-v1-<SHA>.json` — 実装後V3対V1、24試合。
- `benchmarks/issue-58/result-v3-selfplay-<SHA>.json` — 実装後V3自己対戦、4試合。

---

### Task 1: 評価プロトコルを先に固定し、実装前baselineを取得する

**Files:**
- Modify: `scripts/eval_matchup.py:121-128,717-915`
- Modify: `scripts/eval_issue58.py:13-83`
- Modify: `scripts/test_eval_matchup.py`
- Modify: `scripts/test_eval_issue58.py`
- Create: `benchmarks/issue-58/baseline-v3-v1-<SHA>.json`
- Create: `benchmarks/issue-58/baseline-v3-selfplay-<SHA>.json`

**Interfaces:**
- Produces: `build_issue58_match_specs(protocol: str, maps: tuple[str, ...], seeds: tuple[int, ...]) -> list[dict]`
- Produces: `validate_issue58_run(protocol, artifact_stage, maps, subject, baseline, max_turns, seeds, markdown_output, json_output) -> None`
- Produces CLI: `--issue58-protocol {v3-v1,v3-selfplay}` and `--artifact-stage {baseline,result}`
- Metadata additions: `protocol`, `artifact_stage`, `expected_games`, `games_per_seed`, `subject`, `baseline`
- Baseline stage contract: schedule/runtime errors return exit 2; unmet future acceptance criteria do not prevent writing the baseline and do not return exit 1.

- [ ] **Step 1: Write failing schedule and validation tests**

Add tests with exact cardinality and ordering:

```python
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
                {"map": "map_1", "p1": "V3", "p2": "V1", "seed": 58001},
                {"map": "map_1", "p1": "V1", "p2": "V3", "seed": 58001},
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
        self.assertEqual([spec["seed"] for spec in specs], [58001, 58002, 58003, 58004])

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
```

- [ ] **Step 2: Run the focused Python tests and confirm RED**

Run:

```bash
python -m unittest scripts.test_eval_matchup.Issue58PortfolioSchedulingTests scripts.test_eval_issue58.Issue58SeedProtocolTests -v
```

Expected: FAIL because `build_issue58_match_specs` and the expanded validation signature do not exist.

- [ ] **Step 3: Implement protocol-specific deterministic scheduling**

Add these constants and function to `scripts/eval_matchup.py`:

```python
ISSUE58_V3_V1 = "v3-v1"
ISSUE58_V3_SELFPLAY = "v3-selfplay"


def build_issue58_match_specs(protocol, maps, seeds):
    """Issue #58の固定評価プロトコルを決定的な順序で構築する。"""
    if protocol == ISSUE58_V3_V1:
        return build_match_specs(maps, "V3", "V1", seeds)
    if protocol == ISSUE58_V3_SELFPLAY:
        return [
            {"map": map_name, "p1": "V3", "p2": "V3", "seed": seed}
            for map_name in maps
            for seed in seeds
        ]
    raise ValueError(f"unknown Issue #58 protocol: {protocol}")
```

Change `validate_issue58_run` so that it enforces the following exact matrix:

```python
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
if max_turns != 30:
    raise ValueError("Issue #58 requires max_turns=30")
if seeds != (58001, 58002, 58003, 58004):
    raise ValueError("Issue #58 requires seeds 58001,58002,58003,58004")
```

Preserve the existing protection against `matchup_report.md` and identical Markdown/JSON paths.

- [ ] **Step 4: Add CLI arguments and stage-aware exit behavior**

Add parser arguments:

```python
parser.add_argument(
    "--issue58-protocol",
    choices=[ISSUE58_V3_V1, ISSUE58_V3_SELFPLAY],
    help="Fixed Issue #58 evaluation protocol",
)
parser.add_argument(
    "--artifact-stage",
    choices=["baseline", "result"],
    help="Whether this run captures the pre-change baseline or post-change result",
)
```

When `args.criteria == "issue58"`, require both arguments, call `build_issue58_match_specs`, and add the protocol/stage/cardinality fields to metadata. Keep generic criteria on `build_match_specs`.

Split runtime completeness from acceptance completeness so a pre-change baseline can be captured without pretending it satisfies the future design:

```python
runtime_incomplete = (
    len(all_results) != len(match_specs)
    or any(result.get("error") for result in all_results)
)
criteria_incomplete = (
    len(analyses) != len(match_specs)
    or any(not row.get("complete") for row in criteria_rows)
)
execution_incomplete = runtime_incomplete or (
    args.artifact_stage == "result" and criteria_incomplete
)

if args.mode == "batch" and args.criteria == "issue58":
    if execution_incomplete:
        raise SystemExit(2)
    if args.artifact_stage == "result" and not criteria_pass:
        raise SystemExit(1)
```

For `v3-selfplay` baseline capture, do not use analysis-row cardinality as a runtime check because the pre-change analyzer intentionally emits only one V3 perspective per game. Task 11 changes result-stage self-play to require two perspectives.

- [ ] **Step 5: Run Python unit tests and confirm GREEN**

Run:

```bash
python -m unittest scripts.test_eval_matchup scripts.test_eval_issue58 -v
```

Expected: all tests pass.

- [ ] **Step 6: Acquire the V3-vs-V1 baseline before changing production AI logic**

Run from the repository root:

```bash
SHA=$(git rev-parse --short HEAD)
python scripts/eval_matchup.py \
  --mode batch \
  --map map_1,map_2,map_3 \
  --p1 V3 \
  --p2 V1 \
  --criteria issue58 \
  --issue58-protocol v3-v1 \
  --artifact-stage baseline \
  --seeds 58001,58002,58003,58004 \
  --max-turns 30 \
  --json-output "benchmarks/issue-58/baseline-v3-v1-$SHA.json" \
  --output "benchmarks/issue-58/baseline-v3-v1-$SHA.md"
```

Expected: exit 0, JSON `metadata.expected_games == 24`, `len(results) == 24`, and no result has a non-null `error`.

- [ ] **Step 7: Acquire the V3 self-play baseline**

Run:

```bash
SHA=$(git rev-parse --short HEAD)
python scripts/eval_matchup.py \
  --mode batch \
  --map map_3 \
  --p1 V3 \
  --p2 V3 \
  --criteria issue58 \
  --issue58-protocol v3-selfplay \
  --artifact-stage baseline \
  --seeds 58001,58002,58003,58004 \
  --max-turns 30 \
  --json-output "benchmarks/issue-58/baseline-v3-selfplay-$SHA.json" \
  --output "benchmarks/issue-58/baseline-v3-selfplay-$SHA.md"
```

Expected: exit 0, JSON `metadata.expected_games == 4`, `len(results) == 4`, and no result has a non-null `error`.

- [ ] **Step 8: Record immutable baseline identifiers**

Add both artifact names, commit SHA, evaluator SHA-256, MCP SHA-256, dirty-tree flag, game counts, and commands to `benchmarks/issue-58/analysis.md`. Do not interpret the future portfolio criteria yet; record only observed metrics and failures of the pre-change V3.

- [ ] **Step 9: Commit only if the user has explicitly authorized commits**

```bash
git add scripts/eval_matchup.py scripts/eval_issue58.py scripts/test_eval_matchup.py scripts/test_eval_issue58.py benchmarks/issue-58/
git commit -m "test: add issue 58 portfolio baselines"
```

---

### Task 2: 島状態・判断・評価・割当の型と排他的分類を追加する

**Files:**
- Create: `engine/src/ai/island_campaign.rs`
- Modify: `engine/src/ai/mod.rs`
- Test: `engine/src/ai/island_campaign.rs`

**Interfaces:**
- Produces: `IslandCampaignState`, `IslandCampaignDecision`, `IslandCampaignFacts`, `IslandCampaignAssessment`, `IslandCampaignRequirement`, `IslandCampaignAssignment`, `IslandCampaignShortfall`, `IslandCampaignPortfolio`
- Produces: `classify_island(facts: &IslandCampaignFacts) -> IslandCampaignState`
- Later tasks consume exact field names from the design spec plus `state_reason`, `decision_reason`, and assignment Entity-ID vectors.

- [ ] **Step 1: Register the module and write the failing classification table**

Add `pub mod island_campaign;` to `engine/src/ai/mod.rs`, then add this test table to the new file:

```rust
#[test]
fn classifies_islands_in_required_precedence_order() {
    let cases = [
        (facts_without_capturable_properties(), IslandCampaignState::Ignored),
        (facts_with_both_armies_present(), IslandCampaignState::Contested),
        (facts_with_friendly_foothold_and_enemy_eta(2), IslandCampaignState::Threatened),
        (facts_for_empty_neutral_island(), IslandCampaignState::OpenNeutral),
        (facts_for_safe_friendly_foothold(), IslandCampaignState::Secured),
        (facts_for_enemy_foothold(), IslandCampaignState::EnemyHeld),
    ];

    for (facts, expected) in cases {
        assert_eq!(classify_island(&facts), expected);
    }
}
```

Also test that both armies plus `enemy_arrival_eta == Some(1)` is `Contested`, proving `Contested` precedes `Threatened`.

- [ ] **Step 2: Run the focused Rust test and confirm RED**

Run:

```bash
cargo test -p engine ai::island_campaign::tests::classifies_islands_in_required_precedence_order -- --exact
```

Expected: compile failure because the module and types are not implemented.

- [ ] **Step 3: Define the domain types**

Use the exact public assessment fields from the design and additive reason fields:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandCampaignState {
    Ignored,
    OpenNeutral,
    Secured,
    Threatened,
    Contested,
    EnemyHeld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandCampaignDecision {
    Observe,
    Expand,
    Secure,
    Defend,
    Contest,
    Reinforce,
    Withdraw,
    Assault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignFacts {
    pub island_id: IslandId,
    pub capturable_properties: u32,
    pub strategic_production_sites: u32,
    pub roi_production_sites: u32,
    pub neutral_properties: u32,
    pub friendly_properties: u32,
    pub enemy_properties: u32,
    pub friendly_units: u32,
    pub enemy_units: u32,
    pub friendly_combat_value: u32,
    pub enemy_combat_value: u32,
    pub friendly_arrival_eta: Option<u32>,
    pub enemy_arrival_eta: Option<u32>,
    pub friendly_capture_eta: Option<u32>,
    pub enemy_capture_eta: Option<u32>,
    pub transport_eta: Option<u32>,
    pub capture_turns: u32,
    pub island_income_per_turn: u32,
    pub missing_expansion_package_cost: u32,
    pub reachable: bool,
    pub has_unowned_properties: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignAssessment {
    pub island_id: IslandId,
    pub state: IslandCampaignState,
    pub decision: IslandCampaignDecision,
    pub state_reason: String,
    pub decision_reason: String,
    pub neutral_properties: u32,
    pub friendly_properties: u32,
    pub enemy_properties: u32,
    pub friendly_combat_value: u32,
    pub enemy_combat_value: u32,
    pub friendly_arrival_eta: Option<u32>,
    pub enemy_arrival_eta: Option<u32>,
    pub friendly_capture_eta: Option<u32>,
    pub enemy_capture_eta: Option<u32>,
    pub expansion_payback_turns: Option<u32>,
    pub required_budget: u32,
    pub allocated_budget: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignRequirement {
    pub preferred_transport: Option<UnitType>,
    pub transport_slots: u32,
    pub capture_units: u32,
    pub combat_budget: u32,
    pub total_budget: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignAssignment {
    pub island_id: IslandId,
    pub decision: IslandCampaignDecision,
    pub target_position: GridPosition,
    pub requirement: IslandCampaignRequirement,
    pub purchase_shortfall: IslandCampaignRequirement,
    pub allocated_budget: u32,
    pub transport_entities: Vec<Entity>,
    pub capture_entities: Vec<Entity>,
    pub combat_entities: Vec<Entity>,
    pub operation_ready: bool,
    pub continued_from_existing_squad: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignShortfall {
    pub island_id: IslandId,
    pub decision: IslandCampaignDecision,
    pub preferred_transport: Option<UnitType>,
    pub transport_slots: u32,
    pub capture_units: u32,
    pub combat_budget: u32,
    pub priority_rank: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IslandCampaignPortfolio {
    pub islands: Vec<IslandCampaignAssessment>,
    pub active_offensives: Vec<IslandCampaignAssignment>,
    pub defenses: Vec<IslandCampaignAssignment>,
}
```

`operation_ready` is true only when all required transport, capture, and combat Entity IDs already exist. A fully funded `purchase_shortfall` may reserve a future operation slot, but Task 7 must leave its Squad in `Forming` and must not start Transit until `operation_ready` becomes true.

Keep Entity references as ECS IDs; do not store unit references or pointers.

- [ ] **Step 4: Implement the exact precedence classifier**

Implement in this order and return immediately on the first match:

```rust
pub fn classify_island(facts: &IslandCampaignFacts) -> IslandCampaignState {
    if facts.capturable_properties == 0
        || !facts.reachable
        || (facts.island_income_per_turn == 0 && facts.strategic_production_sites == 0)
    {
        return IslandCampaignState::Ignored;
    }
    if facts.friendly_units > 0 && facts.enemy_units > 0 {
        return IslandCampaignState::Contested;
    }
    if facts.enemy_units == 0
        && (facts.friendly_units > 0 || facts.friendly_properties > 0)
        && facts.enemy_arrival_eta.is_some_and(|eta| eta <= 2)
    {
        return IslandCampaignState::Threatened;
    }
    if facts.neutral_properties > 0
        && facts.friendly_properties == 0
        && facts.enemy_properties == 0
        && facts.friendly_units == 0
        && facts.enemy_units == 0
    {
        return IslandCampaignState::OpenNeutral;
    }
    if facts.enemy_units == 0
        && (facts.friendly_units > 0 || facts.friendly_properties > 0)
    {
        return IslandCampaignState::Secured;
    }
    if facts.friendly_units == 0
        && (facts.enemy_units > 0 || facts.enemy_properties > 0)
    {
        return IslandCampaignState::EnemyHeld;
    }
    IslandCampaignState::Ignored
}
```

Document in Japanese why the order is part of the domain rule.

- [ ] **Step 5: Add transition and initial-state tests**

Test these exact transitions by mutating facts, not by storing state:

```rust
assert_eq!(classify_island(&open), IslandCampaignState::OpenNeutral);
open.friendly_units = 1;
open.friendly_properties = 1;
assert_eq!(classify_island(&open), IslandCampaignState::Secured);
open.enemy_arrival_eta = Some(2);
assert_eq!(classify_island(&open), IslandCampaignState::Threatened);
open.enemy_units = 1;
assert_eq!(classify_island(&open), IslandCampaignState::Contested);
```

Also cover own home island=`Secured`, enemy home island=`EnemyHeld`, empty neutral=`OpenNeutral`, no-property island=`Ignored`.

- [ ] **Step 6: Run the module tests and confirm GREEN**

Run:

```bash
cargo test -p engine ai::island_campaign::tests -- --nocapture
```

Expected: all classification and transition tests pass.

- [ ] **Step 7: Commit only if explicitly authorized**

```bash
git add engine/src/ai/mod.rs engine/src/ai/island_campaign.rs
git commit -m "feat: add island campaign domain model"
```

---

### Task 3: ROI、侵攻予算、Contested判断を純粋関数で実装する

**Files:**
- Modify: `engine/src/ai/island_campaign.rs`
- Test: `engine/src/ai/island_campaign.rs`

**Interfaces:**
- Produces: `calculate_expansion_payback_turns(transport_eta, capture_turns, missing_package_cost, island_income_per_turn) -> Option<u32>`
- Produces: `required_assault_budget(enemy_combat_value: u32) -> u32`
- Produces: `decide_contested(facts, reinforced_friendly_power, can_allocate_reinforcement, has_better_open_neutral) -> IslandCampaignDecision`
- Produces: `assess_island(facts: &IslandCampaignFacts) -> IslandCampaignAssessment`

- [ ] **Step 1: Write failing formula boundary tests**

```rust
#[test]
fn calculates_open_neutral_payback_with_ceiling_division() {
    assert_eq!(
        calculate_expansion_payback_turns(Some(2), 3, 6_001, 1_000),
        Some(12),
    );
    assert_eq!(calculate_expansion_payback_turns(Some(2), 3, 6_000, 0), None);
    assert_eq!(calculate_expansion_payback_turns(None, 3, 6_000, 1_000), None);
}

#[test]
fn calculates_enemy_held_budget_floor_and_scaled_budget() {
    assert_eq!(required_assault_budget(0), 32_700);
    assert_eq!(required_assault_budget(8_500), 32_700);
    assert_eq!(required_assault_budget(10_000), 34_500);
}
```

For 10,000G enemy value, combat budget is 12,000G and total is 22,500G + 12,000G = 34,500G.

- [ ] **Step 2: Run focused tests and confirm RED**

Run:

```bash
cargo test -p engine ai::island_campaign::tests::calculates_ -- --nocapture
```

Expected: compile failure because the formula functions do not exist.

- [ ] **Step 3: Implement overflow-safe integer formulas**

```rust
fn ceil_div(numerator: u32, denominator: u32) -> u32 {
    numerator / denominator + u32::from(numerator % denominator != 0)
}

pub fn calculate_expansion_payback_turns(
    transport_eta: Option<u32>,
    capture_turns: u32,
    missing_package_cost: u32,
    island_income_per_turn: u32,
) -> Option<u32> {
    let transport_eta = transport_eta?;
    if island_income_per_turn == 0 {
        return None;
    }
    Some(
        transport_eta
            .saturating_add(capture_turns)
            .saturating_add(ceil_div(missing_package_cost, island_income_per_turn)),
    )
}

pub fn required_assault_budget(enemy_combat_value: u32) -> u32 {
    let scaled_enemy_value = ceil_div(enemy_combat_value.saturating_mul(12), 10);
    22_500u32.saturating_add(10_200u32.max(scaled_enemy_value))
}
```

- [ ] **Step 4: Write failing Contest/Reinforce/Withdraw tests**

Cover all branches:

```rust
assert_eq!(
    decide_contested(&facts_with_capture_etas(Some(3), Some(2)), 10_000, false, false),
    IslandCampaignDecision::Contest,
);
assert_eq!(
    decide_contested(&facts_with_power(8_000, 10_000), 12_000, true, false),
    IslandCampaignDecision::Reinforce,
);
assert_eq!(
    decide_contested(&facts_with_power(8_000, 10_000), 8_000, false, true),
    IslandCampaignDecision::Withdraw,
);
assert_eq!(
    decide_contested(&facts_with_power(8_000, 10_000), 8_000, false, false),
    IslandCampaignDecision::Contest,
);
```

The final branch remains `Contest` because withdrawal is only allowed when a better OpenNeutral investment exists; stranded units continue local defense/capture.

- [ ] **Step 5: Implement decision derivation and reason strings**

Implement the rules in two explicit layers:

`assess_island(facts)` derives state-local values without shared resource knowledge:

- `Ignored` → `Observe`.
- `OpenNeutral` with non-null payback → provisional `Expand`; null payback → `Observe`.
- `Secured` with unowned properties → `Secure`; otherwise `Observe`.
- `Threatened` → `Defend`.
- `Contested` → provisional `Contest`.
- `EnemyHeld` → provisional `Assault` with `required_budget = required_assault_budget(enemy_combat_value)`.

`decide_contested` is called by Task 5 after the shared resource pool and all OpenNeutral candidates are known. It returns `Contest` when `friendly_capture_eta <= enemy_capture_eta + 1` and friendly power ≥ enemy power; otherwise `Reinforce` only when a complete package reaches 120%; otherwise `Withdraw` only when a better OpenNeutral candidate exists; otherwise `Contest`. Task 5 also downgrades provisional `Expand`/`Reinforce`/`Assault` to `Observe` when a complete package cannot be reserved.

Populate `state_reason` and provisional `decision_reason` with stable Japanese messages selected from the actual branch. Task 5 replaces `decision_reason` when it finalizes or rejects an allocation; never use debug formatting of the entire struct.

- [ ] **Step 6: Run all pure-function tests and confirm GREEN**

Run:

```bash
cargo test -p engine ai::island_campaign::tests -- --nocapture
```

Expected: all state, ROI, budget, and contested-decision tests pass.

- [ ] **Step 7: Commit only if explicitly authorized**

```bash
git add engine/src/ai/island_campaign.rs
git commit -m "feat: evaluate island campaign decisions"
```

---

### Task 4: ECS盤面と既存Squadから全島の評価入力を再構築する

**Files:**
- Create: `engine/src/ai/island_campaign_analysis.rs`
- Modify: `engine/src/ai/mod.rs`
- Modify: `engine/src/ai/island_campaign.rs`
- Test: `engine/src/ai/island_campaign_analysis.rs`

**Interfaces:**
- Consumes: `IslandMap`, `Map`, `MasterDataRegistry`, unit `Faction/GridPosition/UnitStats/Health`, `Property`, `SquadManager`, `TurnDistanceCache`
- Produces: `collect_island_campaign_facts(world: &mut World, player_id: PlayerId) -> Vec<IslandCampaignFacts>`
- Produces: `analyze_island_campaign(world: &mut World, player_id: PlayerId) -> IslandCampaignPortfolio`
- Determinism: returned facts and assessments are sorted by `island_id.0`.

- [ ] **Step 1: Write a hand-built ECS test for initial classification**

Follow `engine/src/ai/island_invasion_tests.rs` setup: initialize resources, despawn master-map entities, install a small square `Map`, analyze `IslandMap`, insert `GameRng`, `MatchState`, and `PlayerAiSettings`, then spawn properties and units.

The fixture must contain four coordinate-independent islands discovered by `IslandMap::analyze`:

- own property + own unit → `Secured`;
- enemy property + enemy unit → `EnemyHeld`;
- neutral property + no units → `OpenNeutral`;
- no capturable property → `Ignored`.

Assert by discovered `IslandId`, not hard-coded map_3 coordinates.

- [ ] **Step 2: Run the focused analysis test and confirm RED**

Run:

```bash
cargo test -p engine ai::island_campaign_analysis::tests::reconstructs_initial_state_for_every_island -- --exact
```

Expected: compile failure because the analysis module is absent.

- [ ] **Step 3: Implement deterministic property and unit aggregation**

In `collect_island_campaign_facts`:

1. Clone the lightweight `IslandMap` and `Map` resources before opening ECS queries to avoid overlapping world borrows.
2. Initialize one accumulator per `IslandMap::islands`, sorted by `IslandId`.
3. Count neutral/friendly/enemy properties from `Property::owner_id`.
4. Sum income with `MasterDataRegistry::landscape_income(property.terrain.as_str())`.
5. Count Capital/Factory/Port/Airport that can produce at least one master-data unit in `strategic_production_sites` for the `Ignored` value check. Count only Factory/Port/Airport in `roi_production_sites` for the OpenNeutral tie-break required by the design.
6. Count friendly/enemy units by the island containing their `GridPosition`.
7. Calculate combat value as `stats.cost * current_hp / max_hp`, excluding units whose only strategic role is transport/supply and excluding cargo already represented through its assigned Squad.
8. For a transported unit, use its transport Squad target island as future arrival strength rather than treating the cargo as absent.
9. Sort queried unit snapshots by `Entity::to_bits()` before ETA and reservation processing.

Use saturating arithmetic for accumulated values.

- [ ] **Step 4: Implement transport reachability and ETA without map-specific branches**

For each island:

- A unit already on the island has ETA 0.
- An existing Squad assigned to the island uses its current transport phase and `calculate_turn_distance` to derive ETA.
- Visible enemy ground/air/ship/transport units contribute the minimum reachable ETA to an island tile or valid adjacent landing/coast tile.
- `friendly_capture_eta` is the minimum of each assigned/live capture unit's arrival ETA, movement turns from its landing/current tile to the nearest unowned property, and remaining capture turns derived from that property's current/max capture points.
- `enemy_capture_eta` is calculated the same way for visible enemy capture-capable units against neutral or friendly properties; when no visible capturer has a route, use `None`.
- A missing route yields `None`, which is treated as no imminent threat.
- Set `reachable = true` immediately when the player already has a unit or property on the island; no inter-island transport is required to manage that foothold.
- For islands without a friendly foothold, determine `reachable` by checking both currently held transports and transport types producible at owned facilities.
- Prefer `TransportHelicopter` for OpenNeutral when it can reach and is producible; otherwise choose the reachable transport with minimum `(cost, UnitType deterministic rank)`.
- Do not infer hidden enemy intent; only use current positions and movement capabilities.

Reuse `TerrainConnectivity` and `calculate_turn_distance`; do not duplicate pathfinding.

- [ ] **Step 5: Reconstruct existing operation continuity from SquadManager**

Add the operation snapshot in `island_campaign.rs` with `pub(crate)` visibility so both analysis and allocation use the same type:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingCampaignOperation {
    pub island_id: IslandId,
    pub target_position: GridPosition,
    pub transport_entities: Vec<Entity>,
    pub capture_entities: Vec<Entity>,
    pub combat_entities: Vec<Entity>,
}
```

Build it only from live Squad entities after `update_squads` cleanup. Sort operations by island ID and each Entity vector by `to_bits()`. Mark an operation maintainable until the island is `Secured`, the decision is `Withdraw`, defense preempts it, or all transport/capture capability is lost and no complete replacement package is available.

- [ ] **Step 6: Build assessments for all islands and confirm GREEN**

`analyze_island_campaign` must call `collect_island_campaign_facts`, map every fact through `assess_island`, and initially return all assessments with empty assignment lists. Task 5 fills allocation.

Run:

```bash
cargo test -p engine ai::island_campaign_analysis::tests -- --nocapture
cargo test -p engine ai::islands::tests -- --nocapture
```

Expected: every discovered island has exactly one assessment, sorted by ID; initial states and ETA boundaries pass.

- [ ] **Step 7: Commit only if explicitly authorized**

```bash
git add engine/src/ai/mod.rs engine/src/ai/island_campaign.rs engine/src/ai/island_campaign_analysis.rs
git commit -m "feat: analyze every island from ecs state"
```

---

### Task 5: 完全編成を予約し、Defend優先・最大3攻勢のポートフォリオを構築する

**Files:**
- Modify: `engine/src/ai/island_campaign.rs`
- Modify: `engine/src/ai/island_campaign_analysis.rs`
- Test: `engine/src/ai/island_campaign.rs`

**Interfaces:**
- Produces: `CampaignUnitCandidate { entity, unit_type, cost, can_capture, max_cargo, island_id, assigned_island }`
- Produces: `CampaignResourcePool { available_funds, units }`
- Produces: `pub(crate) IslandCampaignCandidate { assessment, target_position, roi_production_sites, transport_eta, requirement, existing_operation }`
- Produces: `pub(crate) fn allocate_campaign_portfolio(candidates: Vec<IslandCampaignCandidate>, pool: CampaignResourcePool) -> IslandCampaignPortfolio`
- Produces methods: `assignment_for(island_id: IslandId) -> Option<&IslandCampaignAssignment>`, `offensive_target_positions() -> Vec<GridPosition>`, `aggregate_missing_requirements() -> Vec<IslandCampaignShortfall>`

- [ ] **Step 1: Write failing tests for top-three allocation and no double counting**

Create four OpenNeutral assessments with payback values 4, 5, 6, 7 and enough resources for four packages. Assert only the first three become active offensives.

Then use exactly one transport Entity in a pool for two candidates and assert it appears in only one assignment:

```rust
let assigned_ids: Vec<u64> = portfolio
    .active_offensives
    .iter()
    .flat_map(|assignment| assignment.transport_entities.iter())
    .map(|entity| entity.to_bits())
    .collect();
let unique_ids: HashSet<u64> = assigned_ids.iter().copied().collect();
assert_eq!(assigned_ids.len(), unique_ids.len());
```

Also assert `sum(assignment.purchase_shortfall.total_budget) <= initial_available_funds`, and assert a candidate that cannot receive its complete requirement remains `Observe` with `allocated_budget == 0` and a zeroed `purchase_shortfall`.

- [ ] **Step 2: Run allocation tests and confirm RED**

Run:

```bash
cargo test -p engine ai::island_campaign::tests::allocates_ -- --nocapture
```

Expected: compile failure because resource-pool allocation is absent.

- [ ] **Step 3: Define allocation candidates and package requirements**

Add the resource-pool types and internal candidate that carry reservation state and every deterministic tie-break input unavailable from the public assessment alone:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignUnitCandidate {
    pub(crate) entity: Entity,
    pub(crate) unit_type: UnitType,
    pub(crate) cost: u32,
    pub(crate) can_capture: bool,
    pub(crate) max_cargo: u32,
    pub(crate) island_id: Option<IslandId>,
    pub(crate) assigned_island: Option<IslandId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignResourcePool {
    pub(crate) available_funds: u32,
    pub(crate) units: Vec<CampaignUnitCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IslandCampaignCandidate {
    pub(crate) assessment: IslandCampaignAssessment,
    pub(crate) target_position: GridPosition,
    pub(crate) roi_production_sites: u32,
    pub(crate) transport_eta: Option<u32>,
    pub(crate) requirement: IslandCampaignRequirement,
    pub(crate) existing_operation: Option<ExistingCampaignOperation>,
}
```

Move `ExistingCampaignOperation` to `island_campaign.rs` with `pub(crate)` visibility so the ECS analyzer can construct it and the pure allocator can consume it without exposing it outside `engine::ai`.

Use these exact minimums:

- `Expand`: preferred TransportHelicopter, 2 light cargo slots, 2 capture units, 0 combat budget, 6,000G missing-package floor when nothing exists.
- `Secure`: no new offshore package; reuse an island-local capture unit.
- `Defend`: enough unassigned combat value to reach visible arriving enemy value; it is not counted against the offensive cap.
- `Contest`: preserve the current package when Contest conditions already hold.
- `Reinforce`: reserve a complete package that lifts two-turn friendly power to at least `ceil(enemy_power * 1.2)`.
- `Assault`: Lander + TransportHelicopter + 2 capture units + at least 10,200G combat assets, with total required budget from `required_assault_budget`.

Existing suitable unassigned units reduce missing purchase cost at their actual cost. Never reduce a requirement with a unit already assigned to another island.

- [ ] **Step 4: Implement stable priority keys**

Sort without HashMap iteration dependence:

1. `Defend`: `enemy_arrival_eta`, then larger enemy value, then island ID.
2. Existing valid offensives before new offensives of the same decision class.
3. `OpenNeutral`: payback turns ascending; `roi_production_sites` (Factory/Port/Airport) descending; neutral-property count descending; transport ETA ascending; island ID ascending.
4. `Contested`: `Contest` before `Reinforce`, then capture ETA, enemy value, island ID.
5. `EnemyHeld`: required budget ascending, enemy value ascending, island ID ascending.

Represent descending numeric fields with `Reverse`; do not negate unsigned values.

- [ ] **Step 5: Implement reservation with one mutable resource pool**

For each accepted assignment:

1. Select candidate units from a vector sorted by `(already_on_target_island first, cost, UnitType rank, Entity::to_bits())`.
2. Remove selected Entity IDs from the pool immediately.
3. Deduct only the remaining purchase budget from `available_funds` immediately.
4. Reject and roll back the entire provisional reservation when existing Entity value plus available funds cannot cover every transport, capture, and combat requirement. If funds can cover missing units, reserve the whole purchase package in `purchase_shortfall`; never reserve only one fragment of it.
5. Set `operation_ready = purchase_shortfall.transport_slots == 0 && purchase_shortfall.capture_units == 0 && purchase_shortfall.combat_budget == 0`.
6. Populate `allocated_budget` with existing reserved unit value plus reserved purchase funds.
7. Stop adding offensives after three successful fully funded assignments, whether they are ready now or waiting in `Forming` for their complete reserved purchase package.

Use a cloned provisional pool per candidate and replace the real pool only on success; this makes rollback explicit and prevents partial assignment.

- [ ] **Step 6: Implement Threatened preemption**

When a defense cannot be filled from unassigned resources, release the lowest-priority active offensive as a complete assignment, return all its Entity IDs and reserved funds to the pool, then allocate `Defend`. Preserve the other offensives. Mark the released island `Observe` with decision reason `Threatened島防衛のため攻勢を一時停止`.

- [ ] **Step 7: Add deterministic-order tests**

Construct the same candidates in normal and reversed insertion order. Assert the complete `IslandCampaignPortfolio` values are equal, including assignment order and Entity vectors.

Run:

```bash
cargo test -p engine ai::island_campaign::tests -- --nocapture
```

Expected: all allocation, preemption, and determinism tests pass.

- [ ] **Step 8: Wire allocation into `analyze_island_campaign`**

Collect current funds, sorted unit candidates, deterministic target positions, and existing operations. Call `allocate_campaign_portfolio` after all island assessments are known so Withdraw can compare against the best OpenNeutral ROI. Ensure every assessment remains in `portfolio.islands`, including unassigned and secured islands.

- [ ] **Step 9: Commit only if explicitly authorized**

```bash
git add engine/src/ai/island_campaign.rs engine/src/ai/island_campaign_analysis.rs
git commit -m "feat: allocate island campaign portfolio"
```

---

### Task 6: ProductionStrategyを単一InvasionTargetからポートフォリオへ移行する

**Files:**
- Modify: `engine/src/ai/objectives.rs:19-24`
- Modify: `engine/src/ai/strategy.rs:26-56,258-337,551-555,689-713,1088-1150`
- Modify: `engine/src/ai/mod.rs`
- Test: `engine/src/ai/strategy.rs`

**Interfaces:**
- Consumes: `analyze_island_campaign(world, player_id)`
- Changes: remove `InvasionTarget`
- Changes: `ProductionStrategy` gains `pub campaign_portfolio: IslandCampaignPortfolio`
- Produces: production demand derived from `portfolio.aggregate_missing_requirements()`

- [ ] **Step 1: Replace the old strategy tests with failing portfolio expectations**

Replace tests that assert one `invasion_target` with tests that assert:

- every island is assessed;
- the same board with reversed spawn order produces equal assessments and assignment island IDs;
- an affordable OpenNeutral package produces `Expand` before an unaffordable `EnemyHeld` `Assault`;
- an EnemyHeld island below 32,700G available budget remains `Observe`.

Do not assert map_3 coordinates, seed values, or player-order-specific behavior.

- [ ] **Step 2: Run focused strategy tests and confirm RED**

Run:

```bash
cargo test -p engine ai::strategy::tests -- --nocapture
```

Expected: failures reference the old single `invasion_target` behavior.

- [ ] **Step 3: Replace the strategy field and remove old selection code**

Change the struct field to:

```rust
/// V3が毎ターン盤面から再構築する島嶼キャンペーン全体。
pub campaign_portfolio: IslandCampaignPortfolio,
```

Delete `InvasionTarget` from `objectives.rs`. Remove both old blocks in `analyze_strategy` that maintain an active invasion island and select a new enemy island. For V3, call `analyze_island_campaign` exactly once and store its return value. Leave V1 behavior unchanged.

- [ ] **Step 4: Aggregate production demand in priority order**

Call `campaign_portfolio.aggregate_missing_requirements()` to return `IslandCampaignShortfall` rows in this exact order:

1. Threatened defense shortage (`priority_rank = 0`).
2. Existing operation replacement shortage (`priority_rank = 1`).
3. OpenNeutral TransportHelicopter + capture shortage (`priority_rank = 2`).
4. Contested reinforcement shortage (`priority_rank = 3`).
5. EnemyHeld assault shortage (`priority_rank = 4`).
6. Existing generic `DemandMatrix` remains the fallback after campaign rows.

Within one rank, sort by island ID. Set `capture_demand`, `light_transport_demand`, `heavy_transport_demand`, and combat-category demand from these rows. Production must consume rows in order and must not let a lower-priority island consume funds or units already reserved by a higher-priority assignment.

- [ ] **Step 5: Replace V3 target-position derivation**

Where strategy currently turns `invasion_target` into one target vector, use:

```rust
let transport_targets = if is_v3 {
    strategy.campaign_portfolio.offensive_target_positions()
} else {
    strategy.priority_targets.clone()
};
```

Return target positions sorted by `(assignment priority, island_id, y, x)` and deduplicated.

- [ ] **Step 6: Run strategy and objective tests**

Run:

```bash
cargo test -p engine ai::strategy::tests -- --nocapture
cargo test -p engine ai::objectives::tests -- --nocapture
```

Expected: portfolio strategy tests pass; existing Objective ROI tests remain green after only `InvasionTarget` is removed.

- [ ] **Step 7: Commit only if explicitly authorized**

```bash
git add engine/src/ai/objectives.rs engine/src/ai/strategy.rs engine/src/ai/mod.rs
git commit -m "refactor: replace single invasion target"
```

---

### Task 7: SquadManagerを島別Assignmentへ接続する

**Files:**
- Modify: `engine/src/ai/squad.rs:722-741,771-786,985-995`
- Modify: `engine/src/ai/island_invasion_tests.rs`
- Create: `engine/src/ai/island_campaign_tests.rs`
- Modify: `engine/src/ai/mod.rs`

**Interfaces:**
- Consumes: `ProductionStrategy::campaign_portfolio`
- Consumes: `IslandCampaignPortfolio::assignment_for(island_id)`
- Existing Load/Transit/Drop phase functions remain unchanged.

- [ ] **Step 1: Write failing ECS tests for three concurrent islands**

Create four target islands with complete resources for four `Expand` packages. Call the normal `update_squads` / `plan_squads` path and assert:

- exactly three target island IDs appear in offensive Squads;
- every selected island has one transport and two capture members across its assignment/Squads;
- the fourth island has no new offensive Squad;
- no Entity belongs to two island operations.

- [ ] **Step 2: Write failing continuity and partial-withdraw tests**

Set up three active operations, make one island satisfy `Withdraw`, and assert the other two keep the same target islands and live member IDs. Then make one safe island retain neutral properties and assert an island-local capture Squad still targets them after the combat Squad is released.

- [ ] **Step 3: Run focused ECS tests and confirm RED**

Run:

```bash
cargo test -p engine ai::island_campaign_tests -- --nocapture
```

Expected: the current single-island filter prevents the expected assignments.

- [ ] **Step 4: Remove the single-island objective filter**

Replace the `strategy.invasion_target` retain block with a retain condition based on portfolio assignments and `Secure` assessments. An objective is eligible for V3 only when:

- its island has an active assignment; or
- its island is `Secured` with decision `Secure`; or
- it belongs to a local non-offshore objective already supported by legacy behavior.

Use a `HashSet<IslandId>` built from the portfolio, but sort objectives afterward using the existing deterministic key extended with assignment priority.

- [ ] **Step 5: Derive each target from its own assignment**

Replace the single target lookup with:

```rust
let target_position = strategy
    .campaign_portfolio
    .assignment_for(objective.target_island)
    .map(|assignment| assignment.target_position)
    .or_else(|| lowest_unowned_property_on_island(objective.target_island));
```

For transport Squads, set `target_island` and `target` from the assignment owning the selected transport/cargo entities. Never copy the first portfolio assignment into unrelated Squads.

- [ ] **Step 6: Map decisions to existing mission responsibilities**

- `Expand` / `Assault` / offshore `Reinforce`: create or maintain Transport Squads using existing Load/Transit/Drop code. When `operation_ready == false`, keep the operation in `Forming`; do not load or enter Transit until every Entity required by the package exists.
- `Secure`: assign island-local capture units to the nearest remaining unowned property.
- `Contest`: separate capture targets from attack targets; reuse existing Capture and Attack mission types.
- `Withdraw`: do not invent reverse-loading behavior; release only recoverable idle/transported assets, and let stranded units continue local capture/defense.
- `Defend`: retarget unassigned or preempted combat Squads toward the threatened island.

- [ ] **Step 7: Preserve transport-state regression tests**

Run:

```bash
cargo test -p engine ai::island_invasion_tests -- --nocapture
cargo test -p engine ai::island_campaign_tests -- --nocapture
```

Expected: existing Load/Transit/Drop tests remain green; multi-island, continuity, Secure, and partial-withdraw tests pass.

- [ ] **Step 8: Commit only if explicitly authorized**

```bash
git add engine/src/ai/squad.rs engine/src/ai/island_invasion_tests.rs engine/src/ai/island_campaign_tests.rs engine/src/ai/mod.rs
git commit -m "feat: plan squads per campaign island"
```

---

### Task 8: 生産を不足編成へ接続し、装甲車の海上輸送誤計上を修正する

**Files:**
- Modify: `engine/src/ai/strategy.rs:652-713`
- Modify: `engine/src/ai/production.rs:318-337,559-566,661-698`
- Test: `engine/src/ai/strategy.rs`
- Test: `engine/src/ai/production.rs`

**Interfaces:**
- Consumes: portfolio aggregate demands and all offensive target positions
- Produces helper: `sea_transport_capacity(unit_type, stats) -> (light_slots, heavy_slots)`
- V1 scoring and demand behavior remain unchanged.

- [ ] **Step 1: Write a failing regression test for the APC bug**

Create a V3 world with:

- one unreachable OpenNeutral island;
- one owned Recon/装甲車 (`max_cargo == 1`, Infantry loadable);
- no TransportHelicopter or Lander;
- enough funds and a helicopter-producing facility.

Call `let strategy = analyze_strategy(&mut world, PlayerId(1));`, assert `strategy.light_transport_demand >= 1`, and assert the highest transport candidate can be TransportHelicopter. Add a companion V1 test that captures its current behavior without changing it.

- [ ] **Step 2: Run the focused regression and confirm RED**

Run:

```bash
cargo test -p engine ai::strategy::tests::v3_recon_does_not_satisfy_sea_transport_demand -- --exact
```

Expected: FAIL because Recon cargo currently zeroes light sea-transport demand.

- [ ] **Step 3: Separate sea capacity from generic cargo capacity**

For V3 portfolio demand, count only transport types whose movement can cross the required water route:

```rust
fn sea_transport_capacity(unit_type: UnitType, stats: &UnitStats) -> (u32, u32) {
    match unit_type {
        UnitType::TransportHelicopter => (stats.max_cargo, 0),
        UnitType::Lander => (stats.max_cargo, stats.max_cargo),
        _ => (0, 0),
    }
}
```

Use the portfolio requirement's preferred transport and target reachability when reducing demand. Recon and other ground carriers may still satisfy legacy/local ground transport demand, but never offshore demand.

- [ ] **Step 4: Consume cargo against the correct demand after production**

In `production.rs`, replace the generic non-Lander subtraction for V3 with the same semantic split:

- TransportHelicopter subtracts only light offshore demand.
- Lander subtracts heavy first when heavy demand exists, otherwise light.
- Recon and other carriers do not subtract offshore demand.
- V1 retains its existing branch unchanged.

- [ ] **Step 5: Use every assigned island target for transport scoring**

Replace the single V3 target vector with `campaign_portfolio.offensive_target_positions()`. A transport receives useful-target score when it can serve at least one assigned target; targets must be sorted and deduplicated before scoring.

- [ ] **Step 6: Test early OpenNeutral composition**

In an ECS test with 6,000G, a helicopter facility, and no units, assert production planning reserves/selects one TransportHelicopter and two Infantry before Lander, Recon, Tank, or enemy-island assault assets. Test the requirement, not exact map coordinates or seed-specific action order.

- [ ] **Step 7: Run strategy and production tests**

Run:

```bash
cargo test -p engine ai::strategy::tests -- --nocapture
cargo test -p engine ai::production::tests -- --nocapture
```

Expected: APC regression and early package tests pass; V1 tests remain unchanged.

- [ ] **Step 8: Commit only if explicitly authorized**

```bash
git add engine/src/ai/strategy.rs engine/src/ai/production.rs
git commit -m "fix: separate offshore transport demand"
```

---

### Task 9: 生産候補の同点処理をHashMap順から決定論的キーへ変更する

**Files:**
- Modify: `engine/src/ai/production.rs:146-193,242-303`
- Test: `engine/src/ai/production.rs`

**Interfaces:**
- Produces: private `ProductionCandidate { score: u32, facility_position: GridPosition, unit_type: UnitType, cost: u32, max_cargo: u32, can_capture: bool }`
- Produces: `compare_production_candidates(left: &ProductionCandidate, right: &ProductionCandidate) -> std::cmp::Ordering`
- Deterministic order: score descending, facility `(y,x)` ascending, UnitType rank ascending, cost ascending.

- [ ] **Step 1: Write failing reversed-insertion tests for both selection loops**

Build equivalent `UnitRegistry` HashMaps in opposite insertion orders with two producible units receiving equal scores. Assert both the reserve-selection path and greedy-production path choose the same `(facility, UnitType)`.

- [ ] **Step 2: Run focused tests repeatedly and confirm the old implementation is order-dependent**

Run:

```bash
cargo test -p engine ai::production::tests::equal_score_selection_is_insertion_order_independent -- --exact --nocapture
```

Expected: FAIL for at least one insertion order. If the process-local HashMap seed happens to mask the failure, assert directly against two explicitly reversed candidate vectors passed to a new selection helper before implementing it.

- [ ] **Step 3: Extract deterministic candidate selection**

Create a candidate struct containing score, facility position, unit type, cost, cargo, and capture flag. Sort candidates with this exact comparison:

1. score descending;
2. facility y ascending;
3. facility x ascending;
4. stable UnitType rank ascending;
5. cost ascending.

Use one shared selector in both reserve and greedy loops. Do not rely on `HashMap` iteration order and do not change the score formula.

- [ ] **Step 4: Run production tests multiple times**

Run:

```bash
cargo test -p engine ai::production::tests -- --nocapture
cargo test -p engine ai::production::tests -- --nocapture
cargo test -p engine ai::production::tests -- --nocapture
```

Expected: all three runs produce the same passing result.

- [ ] **Step 5: Commit only if explicitly authorized**

```bash
git add engine/src/ai/production.rs
git commit -m "fix: make production tie breaks deterministic"
```

---

### Task 10: 島別判断をMCP診断テレメトリへ公開する

**Files:**
- Modify: `engine/src/ai/island_campaign.rs`
- Modify: `engine/src/ai/strategy.rs`
- Modify: `mcp-server/src/invasion_trace.rs`
- Modify: `mcp-server/src/main.rs:456-520`
- Test: `mcp-server/src/invasion_trace.rs`

**Interfaces:**
- Produces engine resource: `IslandCampaignDiagnostics { by_player: HashMap<PlayerId, IslandCampaignPortfolio> }`
- Produces MCP DTO: `IslandCampaignSnapshot` and `IslandCampaignAssignmentSnapshot`
- `simulate_ai_turn` adds `island_campaign` without removing existing response fields.

- [ ] **Step 1: Write a failing serialization test**

Build a portfolio with one assessment and one assignment, snapshot it, serialize with `serde_json`, and assert keys for:

```text
island_id, state, decision, state_reason, decision_reason,
neutral_properties, friendly_properties, enemy_properties,
friendly_combat_value, enemy_combat_value,
friendly_arrival_eta, enemy_arrival_eta,
friendly_capture_eta, enemy_capture_eta,
expansion_payback_turns, required_budget, allocated_budget,
transport_entity_ids, capture_entity_ids, combat_entity_ids,
purchase_shortfall, operation_ready, continued_from_existing_squad
```

- [ ] **Step 2: Run the MCP test and confirm RED**

Run:

```bash
cargo test -p mcp-server invasion_trace::tests::serializes_island_campaign_snapshot -- --exact
```

Expected: compile failure because the DTO is absent.

- [ ] **Step 3: Add a diagnostics-only resource updated by strategy analysis**

Define:

```rust
#[derive(Resource, Debug, Clone, Default)]
pub struct IslandCampaignDiagnostics {
    pub by_player: HashMap<PlayerId, IslandCampaignPortfolio>,
}
```

After V3 computes its portfolio, overwrite only that player's diagnostics entry. This resource is not read by AI decisions and does not carry operation lifecycle state; it is a last-analysis snapshot for observability.

- [ ] **Step 4: Convert engine types to stable MCP DTOs**

Use strings from explicit enum matches (`"OpenNeutral"`, `"Expand"`) rather than Rust debug output. Convert Entity IDs with `to_bits()`. Sort islands by ID, assignments by island ID, and Entity ID vectors numerically before serialization.

- [ ] **Step 5: Return one campaign snapshot per simulated player turn**

At the end of `simulate_ai_turn`, read the latest diagnostics for `active_player_id` and include:

```json
{
  "island_campaign": {
    "player_id": 1,
    "islands": [],
    "active_offensives": [],
    "defenses": []
  }
}
```

Keep `invasion_events`, `transport_squads`, and metrics unchanged so older evaluators remain compatible.

- [ ] **Step 6: Add missing/no-diagnostics boundary tests**

For V1 or a turn where no analysis ran, return `island_campaign: null`; do not fabricate empty state as if all islands had been assessed.

- [ ] **Step 7: Run MCP and engine tests**

Run:

```bash
cargo test -p mcp-server invasion_trace::tests -- --nocapture
cargo test -p engine ai::island_campaign -- --nocapture
```

Expected: serialization, missing diagnostics, and engine tests pass.

- [ ] **Step 8: Commit only if explicitly authorized**

```bash
git add engine/src/ai/island_campaign.rs engine/src/ai/strategy.rs mcp-server/src/invasion_trace.rs mcp-server/src/main.rs
git commit -m "feat: expose island campaign telemetry"
```

---

### Task 11: Python評価をポートフォリオ受け入れ条件とbaseline比較へ移行する

**Files:**
- Modify: `scripts/eval_matchup.py:165-360,854-882`
- Modify: `scripts/eval_issue58.py:189-700`
- Modify: `scripts/test_eval_matchup.py`
- Modify: `scripts/test_eval_issue58.py`

**Interfaces:**
- Game result adds: `island_campaign_history: list[dict]`
- Produces: `analyze_issue58_player(game, player_number, protocol) -> dict`
- Produces: `analyze_issue58_game(game, protocol) -> list[dict]` (one row for V3-vs-V1, two rows for self-play)
- Produces: `compare_issue58_baseline(result_payload, baseline_payload) -> list[dict]`

- [ ] **Step 1: Write failing collection and two-sided analysis tests**

Mock two `simulate_ai_turn` responses, one per player, each with different `island_campaign`. Assert `run_single_game` stores turn/player-tagged snapshots. For self-play, assert `analyze_issue58_game` returns two analyses with `subject_player` 1 and 2. For V3-vs-V1, assert it returns only the side running V3.

- [ ] **Step 2: Write failing hard-failure tests**

Create synthetic histories and assert FAIL for each independent condition:

- one expected island missing from a turn;
- more than three `Expand/Contest/Reinforce/Assault` island IDs in one player turn;
- one Entity ID appearing in two assignments in one player turn;
- allocated budget exceeding available funds plus eligible unassigned assets;
- EnemyHeld `Assault` with `allocated_budget < required_budget`;
- any game `error`;
- self-play with only one player analysis.

- [ ] **Step 3: Run Python tests and confirm RED**

Run:

```bash
python -m unittest scripts.test_eval_matchup scripts.test_eval_issue58 -v
```

Expected: failures for absent campaign history and single-sided analysis.

- [ ] **Step 4: Collect campaign history without dropping existing traces**

After each `simulate_ai_turn`, append a normalized entry:

```python
campaign = response.get("island_campaign")
if campaign is not None:
    island_campaign_history.append(
        {
            "round": completed_round,
            "turn": state.get("turn"),
            "player_id": response.get("player_id"),
            "campaign": campaign,
        }
    )
```

Return it alongside `invasion_events`, `transport_history`, and `strategic_history`.

- [ ] **Step 5: Implement per-player behavioral checks**

For every V3 player, compute and report:

- complete island-state coverage per recorded turn;
- initial neutral islands classified `OpenNeutral`;
- first offensive target and whether it was ROI-ranked OpenNeutral before EnemyHeld;
- maximum simultaneous offensive count;
- duplicate funds/unit assignments;
- Contested decision/reason presence;
- observed `Secured` after enemy removal even with unowned properties;
- continued `Secure` behavior;
- `Threatened` transition at enemy ETA ≤ 2 and defense preemption;
- EnemyHeld assault budget compliance;
- first initial-island-external property capture.

Do not infer hidden state from map_3 coordinates. Use initial/final board states, island IDs, ownership, and telemetry.

- [ ] **Step 6: Keep comparison criteria separate from self-play behavior criteria**

For `v3-v1`, retain both player-order buckets and enforce map_3:

- average V3 income ≥ V1;
- average V3 properties ≥ V1;
- average V3 ZOC > V1;
- V3 captures an initial-island-external property;
- win rate ≥ 40%;
- average thinking time ≤ 150% of the corresponding baseline artifact;
- no early or underfunded EnemyHeld assault;
- first offshore expansion is normally OpenNeutral with helicopter/capture package.

Use map_1/map_2 as non-decline regression rows.

For `v3-selfplay`, require four games and eight player analyses. Do not use winner as the primary criterion; require all hard behavioral checks and both players to capture at least one initial-island-external property.

- [ ] **Step 7: Implement baseline/result metadata comparison**

Add a required `--baseline-json` argument for `artifact_stage=result`. Load it and verify:

- protocol matches;
- seeds match exactly;
- baseline stage is `baseline`;
- expected game count matches;
- evaluator/MCP hashes are recorded;
- result SHA and artifact path differ from the baseline artifact;
- comparison uses the baseline thinking-time distribution for the same map and player order.

Reject mismatched artifacts before judging criteria.

- [ ] **Step 8: Update report generation**

Generate separate sections for protocol metadata, schedule completeness, per-map comparison, per-player behavior, hard failures, baseline comparison, and artifact paths. Preserve JSON as the source of truth and generate Markdown from the same payload.

- [ ] **Step 9: Run Python tests and confirm GREEN**

Run:

```bash
python -m unittest scripts.test_eval_matchup scripts.test_eval_issue58 -v
```

Expected: all schedule, metadata, collection, behavior, hard-failure, and report tests pass.

- [ ] **Step 10: Commit only if explicitly authorized**

```bash
git add scripts/eval_matchup.py scripts/eval_issue58.py scripts/test_eval_matchup.py scripts/test_eval_issue58.py
git commit -m "test: evaluate island campaign portfolio"
```

---

### Task 12: 全体統合、実装後評価、分析更新、品質ゲートを完了する

**Files:**
- Modify: `benchmarks/issue-58/analysis.md`
- Create: `benchmarks/issue-58/result-v3-v1-<SHA>.json`
- Create: `benchmarks/issue-58/result-v3-selfplay-<SHA>.json`
- Potentially modify only files implicated by failing tests; do not broaden scope.

**Interfaces:**
- Consumes both Task 1 baseline JSON artifacts.
- Produces final comparison and self-play result artifacts.

- [ ] **Step 1: Run formatting before the expensive suites**

Run:

```bash
cargo fmt --all
cargo fmt --all -- --check
```

Expected: check exits 0.

- [ ] **Step 2: Run focused Rust suites**

Run:

```bash
cargo test -p engine ai::island_campaign -- --nocapture
cargo test -p engine ai::island_campaign_tests -- --nocapture
cargo test -p engine ai::island_invasion_tests -- --nocapture
cargo test -p engine ai::strategy::tests -- --nocapture
cargo test -p engine ai::production::tests -- --nocapture
cargo test -p mcp-server invasion_trace::tests -- --nocapture
```

Expected: all focused suites pass with no ignored new acceptance tests.

- [ ] **Step 3: Run the complete Python evaluation test suite**

Run:

```bash
python -m unittest scripts.test_eval_matchup scripts.test_eval_issue58 -v
```

Expected: all tests pass.

- [ ] **Step 4: Run full workspace tests and lint gates**

Run exactly:

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Expected: all commands exit 0. Fix only issues caused by this plan; report unrelated pre-existing failures separately rather than suppressing them.

- [ ] **Step 5: Produce the post-change V3-vs-V1 artifact**

Resolve the exact Task 1 artifact and fail rather than guessing when zero or multiple candidates exist, then run:

```bash
BASELINE_V3_V1_JSON=$(python - <<'PY'
from pathlib import Path
paths = sorted(Path("benchmarks/issue-58").glob("baseline-v3-v1-*.json"))
if len(paths) != 1:
    raise SystemExit(f"expected exactly one V3-vs-V1 baseline, found {len(paths)}: {paths}")
print(paths[0].as_posix())
PY
)
SHA=$(git rev-parse --short HEAD)
python scripts/eval_matchup.py \
  --mode batch \
  --map map_1,map_2,map_3 \
  --p1 V3 \
  --p2 V1 \
  --criteria issue58 \
  --issue58-protocol v3-v1 \
  --artifact-stage result \
  --baseline-json "$BASELINE_V3_V1_JSON" \
  --seeds 58001,58002,58003,58004 \
  --max-turns 30 \
  --json-output "benchmarks/issue-58/result-v3-v1-$SHA.json" \
  --output "benchmarks/issue-58/result-v3-v1-$SHA.md"
```

Expected: 24 completed games, no errors or missing metrics, comparison criteria PASS, exit 0.

- [ ] **Step 6: Produce the post-change self-play artifact**

Resolve the exact Task 1 self-play artifact with the same uniqueness check, then run:

```bash
BASELINE_V3_SELFPLAY_JSON=$(python - <<'PY'
from pathlib import Path
paths = sorted(Path("benchmarks/issue-58").glob("baseline-v3-selfplay-*.json"))
if len(paths) != 1:
    raise SystemExit(f"expected exactly one V3 self-play baseline, found {len(paths)}: {paths}")
print(paths[0].as_posix())
PY
)
SHA=$(git rev-parse --short HEAD)
python scripts/eval_matchup.py \
  --mode batch \
  --map map_3 \
  --p1 V3 \
  --p2 V3 \
  --criteria issue58 \
  --issue58-protocol v3-selfplay \
  --artifact-stage result \
  --baseline-json "$BASELINE_V3_SELFPLAY_JSON" \
  --seeds 58001,58002,58003,58004 \
  --max-turns 30 \
  --json-output "benchmarks/issue-58/result-v3-selfplay-$SHA.json" \
  --output "benchmarks/issue-58/result-v3-selfplay-$SHA.md"
```

Expected: 4 completed games, 8 complete player analyses, no errors/state gaps/double allocations, behavioral criteria PASS, exit 0.

- [ ] **Step 7: Update the analysis with evidence, including failures if any**

Record:

- exact baseline and result filenames and commit SHAs;
- exact commands;
- 24/24 and 4/4 completion counts;
- map_3 income/property/ZOC/win-rate/thinking-time comparisons;
- first offshore expansion package and target state;
- maximum concurrent offensives;
- EnemyHeld assault budget observations;
- Threatened preemption and Secured continuation observations;
- map_1/map_2 regression results;
- all test/clippy/fmt outputs;
- any unmet criterion with the concrete game/seed/order and trace evidence.

Do not claim Issue #58 PASS unless both result artifacts report `overall_pass: true` and every quality gate in Steps 1-4 exited 0.

- [ ] **Step 8: Inspect the final diff for prohibited behavior**

Run:

```bash
git diff --check
git diff -- engine/src/ai scripts mcp-server/src benchmarks/issue-58 docs/superpowers
```

Confirm manually that production AI contains no map_3 coordinates, seed checks, player-order branches, V1 behavior changes, duplicated transport phase logic, or new persistent campaign lifecycle state.

- [ ] **Step 9: Commit only if explicitly authorized**

```bash
git add engine/src/ai mcp-server/src scripts benchmarks/issue-58 docs/superpowers/plans/2026-07-28-issue-58-island-campaign-portfolio.md
git commit -m "feat: implement island campaign portfolio"
```

- [ ] **Step 10: Use the finishing workflow only after fresh verification**

Invoke `superpowers:verification-before-completion`, then `superpowers:finishing-a-development-branch`. Because integration is outward-facing and hard to reverse, present the prescribed branch options and wait for the user's choice; do not push, merge, or create a PR without that explicit choice.
