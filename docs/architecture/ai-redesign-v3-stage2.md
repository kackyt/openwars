# AI再設計v3 段階2（前半）: 部隊再編の段分割と方策enum

[提案書](ai-redesign-v3-proposal.md) §B と段階2のうち、挙動を変えない（R）部分の前半を実装した記録。
基準は段階1の仕上げコミット `c6df306`。

## やったこと

### 1. `plan_squads_with_hooks`（1関数・約1190行）を段に分ける

再編本体を `engine/src/ai/squad/formation.rs` に移し、次の段の関数にした。各段は前段の結果だけを受け取る。

| 段 | 関数 | 入力 → 出力 |
|---|---|---|
| 1 準備 | `update_squads`・`prepare_squads_for_replanning` | 前手番のSquad → 再計画前のSquad |
| 2 分析と採用 | `analyze_strategy_for_formation`・`PlanningPolicy::adopt_portfolio` | 盤面 → `ProductionStrategy`・採用portfolio |
| 3 campaign編成 | `form_campaign_squads`（＋V4配備） | 採用portfolio → `CampaignFormation`（優先順assignment・予約Entity・封鎖島・停止島） |
| 4 汎用編成 | `FormationBoard::observe`・`FreeUnits::collect`、`assign_generic_transports`・`unload_unreserved_loaded_transports`・`assign_capture_depth_transports`・`form_capital_defense_squads`・`form_capture_squads`・`form_attack_squads`・`absorb_surplus_combat_units` | 盤面の事実＋free unit → Squad |
| 5 仕上げ | `finalize_formation` | V4所属修復 → 正規化 → Roadmap観測 |

盤面の事実（地図・占有・物件の所有・島の分類・自首都）は `FormationBoard` に1回だけ集め、
汎用編成の各段は読み取り専用で受け取る。距離cacheは `FormationCaches` にまとめた。

### 2. 8個の前後hook（`SquadPlanningHooks`）を方策enumへ

trait と `MainPlanningHooks` / `ExtendedPlanningHooks` を廃止し、
`formation::PlanningPolicy { Standard, SmallMap }` にした。通常/小規模V4で違うのは次の6点だけで、
すべてこのenumのメソッドにある。

- 採用（`adopt_portfolio`）: 通常はそのまま、小規模V4は首都ルートDAGとRoadmapで選ぶ
- 局地割当（`prepare_local_assignment`）
- 旧首都防衛の有無（`uses_legacy_capital_defense`）
- 占領目標の順序（`sort_capture_targets`）と占領要員の選択（`select_capture_infantry`）
- Roadmap観測の位置（`before_final_reconcile` / `after_final_reconcile`）と、小規模V4の事前処理

小規模V4側の本文は `squad/extended.rs` の自由関数（`adopt_portfolio`・`sort_small_map_capture_targets`・
`select_nearest_capture_infantry`）にした。提案書は「2つの方策enum」としていたが、採用と汎用編成の差は
常に同じ経路で切り替わるため、1つのenumにまとめた。

### 3. 使われない束縛の削除

- `my_infantry_islands`（計算するだけで読まれていなかった）
- `tactical_reserved_entities`（常に空。配備Squad準備へは空集合を直接渡す）

## 検証

| 項目 | 結果 |
|---|---|
| 判断fingerprint | V2/V3/V4 × map_1/2/6/25 × seed1,2 × 両手番 × 30T の48条件で、行動列hashと終局状態が `c6df306` と完全一致 |
| `cargo test --release -p engine --lib` | 872成功・1 ignore（既存）・失敗0。再編の契約テスト5件を含む |
| `cargo clippy --all-targets --all-features -- -D warnings` / `cargo fmt --all -- --check` | 成功 |

契約テストのうち、hookへ記録用の実装を注入して呼出順を検査していたテストは、注入点がなくなったため
「共通の段がSquadManagerを戻し、採用portfolioと編成済みを手番cacheへ記録する」ことを
両方策で直接検査するテストに置き換えた。

## 構造・複雑度（`scripts/measure_ai_complexity.py`、テスト除外）

| 指標 | 変更前 | 変更後 |
|---|---:|---:|
| 再編の最大関数の直接CC | 148（`plan_squads_with_hooks`） | 23（`load_generic_transport`） |
| 同・直接認知的複雑度 | 373 | 51 |
| 入口関数の直接CC | 148 | 5（`plan_squads_with_policy`） |
| 同名の通常/extended関数ペア | 37 | 29 |
| 禁止方向の参照行 | 0 | 0 |

## まだやっていないこと（段階2の後半）

提案書 §B のうち次は未着手。どれも別コミットで扱う。

- R: 盤面snapshot（`context::PlayerBoard`）を戦略分析と編成で共有する（占有モデル2種を明示して維持）
- R: `analyze_strategy_internal`（685行）を状況判断・campaign提案・生産需要へ分け、Squad編成では生産需要を計算しない
- R: 編成の propose/commit 分離（Squadの作成・追加を提案として返し、1つの更新口で確定する。SquadIdの発番順は保つ）
- R: 地形連結cache（campaign編成用と汎用編成用）の統合
- S: 編成後のRoadmap 2回目の選択を観測だけにする・経路の精錬と観測を1回にする・汎用輸送を採用portfolioへ・旧首都防衛の削除
