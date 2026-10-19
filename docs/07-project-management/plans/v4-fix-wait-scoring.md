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

---

## 実装・検証記録（2026-10-19）

- リモートに `fix/wait-scoring` が存在しなかったため、計画書を含む `wip/wait-scoring`（`c655ab0`）を取得し、ローカルの `fix/wait-scoring` で実装。
- `TacticalBoard::own_capitals` と生産ルールの `is_within_production_range` を使い、回復拠点の生産施設判定を首都圏に限定。
- 接近点を消費MPによる連続ターンで算出し、最寄り目標の比較を含めて格子距離をSquare/Hex対応に統一。
- 回帰テストで首都から3/4マス、首都なし、敵・中立施設、Square/Hexの距離、48→49MP境界での前進（+750点）、目標到達300,000点、移動力0、到達不能を検証。
- 指定の `tactics::v2`（15件）、engineの `probe`（16件）、MCPの `wait_probe`（1件）が成功。
- `cargo clippy --all-targets --all-features -- -D warnings`、`cargo fmt --all -- --check` が成功。
- 比較用バイナリ・JSON・JSONLと集計スクリプトは `.tmp/fix-production-and-approach/` に保存（Git管理外）。

### 固定条件の対戦比較

`map_6 / seed 4 / Hex / P1 V200 / P2 V4 / 50T` を、取得時コミットを隔離したworktreeからビルドした `ref-original` と修正後の `fixed` で実行。バイナリのSHA-256と条件を `.tmp/fix-production-and-approach/manifest.json` に記録した。

| 指標 | 修正前 | 修正後 |
| --- | ---: | ---: |
| 首都圏外で -20,000点が適用された同位置Wait（発行済みWaitのプローブ） | 10 | 0 |
| 中央 x=12〜20, y=5〜15 で5回連続のラウンド末記録が同位置だったユニット数 | 49 | 45 |

- 両対戦とも実行エラーなし、50T時点の判定はP2勝利。不当減点は解消したが、中央の停滞は残る。この集計は全兵種・任務を対象とし、正当な防衛待機も含むため、停滞の完全解消を示すものではない。
- 計画書の以前の集計（347件／42体）とは観測範囲・集計方法が一致しないため、直接比較せず今回の同一条件で比較した。
- 修正前の軽戦車 `4294967425` がT12〜16に `(17,7)` へ停滞する事例は再現。修正後は生産・行動履歴が変わり同IDが戦闘ヘリになるため、IDによる一対一比較はできない。48→49MP・2マス前進の評価差が+750点になることは回帰テストで確認した。
- Squareでも修正後50Tを実行し、不当減点0件・実行エラーなしを確認した。

### 既存の対戦記録

Hex距離・接近点の変更により、V2/V3の移動先とV4小規模マップの移動先が変化した。実際の行動列を別ディレクトリに出力して差分を確認し、`v1_vs_v2_map1`、`v3_vs_v4_map6`、`v4_small_map1`、`v4_small_map1_save_load` の4つのgoldenを更新した。V100/V200とV4標準マップのgoldenは変更なし。セーブ／ロードの状態・継続結果の一致判定は維持している。

### 最終検証

`cargo test` が全件成功（1,057件成功、既存のignore 2件）。30Tの同一条件2局比較を含む決定性テスト5件、更新後のgoldenテスト8件、セーブ／ロード検証、doc testを含む。最終差分の `git diff --check` も成功。

---

## B（接近スコア）の取り下げ（2026-10-19）

`map_6 seed1〜8（P1 V200 / P2 V4）` と回帰セット（`map_1/2/25/32/42 × seed1,2 × 先後`、Hex、50T）で、A・B を分けた4変種を比較した。

| 変種 | map_6 撃破勝ち（8局） | map_6 T41-50 敵首都までの平均距離 | 回帰セット V4 勝利（20局） |
| --- | ---: | ---: | ---: |
| 修正前（wip） | 0 | 17.0 | 17 |
| A のみ | 2 | 15.0 | 16 |
| A ＋ B1（Hex距離化） | 2 | 15.2 | 13 |
| A ＋ B1 ＋ B2（連続ターン化） | 3 | 14.9 | 11 |

- map_6 の前進改善はほぼ A だけで得られ、B は回帰セットの勝率を下げた（B1 で −3、B2 で −2）。
- このため B を取り下げ、A（首都圏外の生産施設を生産枠封鎖とみなさない）のみを採用した。
- 待機マスの防御効果は全変種で隣接マス平均を下回り（map_6 −3.3／回帰 −0.9）、地形が待機位置の選択に効いていない問題は B と無関係に残っている。

### 残課題
- `consider_wait`（`tactics/v2.rs`・`tactics/small_map.rs`）は campaign 所属unitの後退を格子距離で判定するため、山を迂回する前進を候補から外している（map_6 の1局で `wait_move_away_from_campaign` 却下 14,133件）。経路長での判定に置き換える。
- 敵ユニットが目標への入口を塞ぐと `calculate_turn_distance` が近似値（`50 + マンハッタン/4`）へ落ち、評価が直線距離に退化する。敵マスを高コストで通過可能とみなした距離で代替する。
- 地形効果の重み（`地形効果×10`）が前進1マス分（約+750点）より桁で小さく、同点比較にしか効かない。
- 首都の狭い入口へ130〜200体が向かう割り当てによる渋滞。
