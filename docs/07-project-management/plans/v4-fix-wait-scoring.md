# 前線都市の不当減点解消と接近スコア連続化による前進・合流改善計画

## Context
プローブによる実測と分析により、以下の2つの根本問題が特定された：
1. **前線都市への -20,000点 減点バグ（渋滞の最大要因）**:
   - ゲームルール上、生産可能な自軍施設は「自軍首都から3マス以内（square/hexトポロジー準拠）」に限られる（`engine/src/systems/production.rs:18`）。
   - しかしAIの待機評価（`scoring.rs::repair_site_at`）では距離を考慮せず全自軍施設を `is_production_facility` で判定していたため、首都から遠く離れた前線の占領済み都市・工場を「生産枠封鎖」と誤認し、通過・待機に `-20,000点` の莫大なペナルティを課していた。
   - これにより、中央の前線都市群が「進めない壁」となり、先頭車両が停止して後続が40体以上スタックする大規模な渋滞を引き起こしていた。
2. **接近点不変（相殺）バグ**:
   - `approach_bonus` は `TurnDistance` の `turns`（`div_ceil` で切り上げられた整数）しか受け取っていない。
   - 目標へ2マス前進しても、途中の迂回コスト（山岳・森）で `used_mp` がわずか1増えてターン数切り上げ境界（例: 48 MP → 49 MP で 12ターン → 13ターン）を跨ぐと、ターン減点（-1,000点）が発生する。
   - 一方、格子距離短縮による加点は軽戦車（移動力4）で 2マス短縮時 +1,000点であり、ターン減点（-1,000点）と完全に相殺（0点差）していた。
   - その結果、現在地（森: 防御+200点）と前進先（道路: 防御+0点）の地形防御点差（200点差）だけで「動かずに森に留まる方が200点高い」と判定され、ユニットが目標へ前進できなくなっていた。また、Square前提の `manhattan` 距離決め打ちにより Hex トポロジーとの不整合も生じていた。

本計画ではこの2つの原因を解消し、前線都市の通過と目標への前進を正常化させ、部隊の集合・合流・連携を成立させる。

---

## 1. 修正内容

### A. 生産施設判定の首都圏限定化 (`engine/src/ai/tactics/board.rs`, `scoring.rs`)
- **自軍首都位置の取得**:
  - `TacticalBoard` に `pub(crate) fn own_capitals(&self) -> Vec<GridPosition>` を追加。
  - `board.properties` から `terrain == Terrain::Capital && owner == Some(self.player_id)` を抽出。
- **`repair_site_at` の生産拠点判定の厳格化**:
  - `board.registry.is_production_facility(terrain)` に加え、`crate::systems::production::is_within_production_range(&capitals, pos.x, pos.y, board.map.topology)` を満たす場合のみ `Some(true)`（生産施設、回復不要時 -20,000点）を返す。
  - 首都から3マス超の前線都市・工場は `Some(false)`（一般回復拠点、回復不要時 -2,000点）となる。
  - これにより、前線都市を通る前進が `-20,000点` で理不尽にブロックされるバグを根絶する。

### B. 接近スコアの連続ターン化とトポロジー対応 (`engine/src/ai/tactics/approach.rs`, `scoring.rs`)
- **トポロジー対応距離の提供**:
  - `ApproachDistances` に `pub(crate) fn distance(&self, a: GridPosition, b: GridPosition) -> i32` を追加し、`self.board.map.distance(a.x, a.y, b.x, b.y) as i32` を呼ぶ（Square / Hex 両対応）。
  - `scoring.rs` 内の `manhattan` 呼び出し（`squad_target_approach_score`, `reference_point_approach_score`, `solo_hunt_score`）を `distances.distance` に置換。
- **`approach_bonus` の連続値化**:
  - シグネチャを `approach_bonus(dist: TurnDistance, grid_distance: i32, max_movement: u32) -> i32` に更新。
  - 到達可能時（`dist.turns < UNREACHABLE_TURNS`）は、整数 `turns` ではなく `cont_turns = dist.used_mp as f32 / max_movement.max(1) as f32` を使用：
    ```rust
    let turn_score = (100.0 - cont_turns).max(0.0) * 1000.0;
    let moves = grid_distance as f32 / max_movement.max(1) as f32;
    let move_score = (100.0 - moves).max(0.0) * 2000.0;
    (turn_score + move_score) as i32
    ```
  - 目標・射程到達時（`used_mp = 0, grid_distance = 0`）は従来どおり **300,000点満点** を維持し、既存の契約・テストを保護。
  - コストがわずか1増えても減点は -250点（軽戦車の場合）にとどまり、格子距離短縮（2マスで +1,000点）が確実に上回るため、差引 +750点の加点となり、森の地形防御点（+200点）に負けず前進が選ばれる。

---

## 2. 影響範囲と互換性の確認
- **V1**: `tactics/` 配下を使用しないため影響ゼロ。
- **V2 / V3 / V4**: `tactics/scoring.rs` と `approach.rs` の共通関数を健全化するため、前線都市での不当な立ち往生が全バージョンで解消される。
- **小規模マップ (`small_map.rs`)**: `small_map.rs` も `repair_site_at` を共通利用しているため、同様に前線都市スタックが解消される。
- **既存テスト**: 目標到達時（300,000点）や射程内同点アサート（`used_mp = 0`）は完全に互換性を保つ。

---

## 3. 検証計画

### Step 1: 単体テストと静的検査
- `cargo test -p engine --lib tactics::v2 -- --test-threads=1`
- `cargo test -p engine --lib probe -- --test-threads=1`
- `cargo test -p mcp-server wait_probe -- --test-threads=1`
- 以下の新規ユニットテストを追加して検証：
  1. `test_repair_site_production_range`: 首都3マス以内の自軍都市が `Some(true)`、首都4マス以上の前線都市が `Some(false)` と判定されること。
  2. `test_approach_bonus_continuous`: 軽戦車の前進時に整数ターン切り上げ境界による相殺が発生せず、スコアが単調増加して地形防御点（+200点）を上回ること。
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo fmt --all -- --check`

### Step 2: 変更前後の比較対戦（map_6 seed4, P2 V4 vs V200, 50T）
- 新バイナリをビルドして固定（`.tmp/fix-production-and-approach/`）。
- 修正前の `ref` バイナリと比較し、以下を確認：
  1. 前線都市上での -20,000点 による同位置Wait（前回 347件）が激減またはゼロになっているか。
  2. 中央付近（x 12〜20, y 5〜15）で 5手番以上スタックしていたユニット数（前回 42体）が解消され、前進しているか。
  3. 軽戦車 4294967425（T13）が (17,7)森 に停滞せず、(15,7) へ前進しているか。
