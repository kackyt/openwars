# Change: 攻撃アクション（直接攻撃・間接攻撃・反撃）の実装

## Why
現在、ユニットの移動とプロパティの占領は実装済みだが、ユニット同士の戦闘が完全に未実装。
攻撃・反撃・弾薬管理を導入することで、ゲームの基本的な戦闘ループを実現する。

## What Changes
- `Unit` に `has_moved` フラグを追加し、移動完了と行動完了を分離する（`action_completed` は攻撃完了に再定義）
- `MatchState` に `attack` メソッドを追加する
  - 攻撃者が範囲内の敵ユニットを攻撃できる
  - `min_range == 1` な武器は直接攻撃・長距離の両方に使える兵器も存在する（直接/間接は排他ではない）
  - 主武器（ammo1）が優先され、弾切れまたは相手にダメージ0 の場合は副武器（ammo2）が使われる
  - 反撃は直接攻撃に対してのみ発生する。防衛側が使用できる武器で攻撃者へのダメージが0になる場合は反撃不可（弾薬消費しない）
  - **直接攻撃のダメージ解決は同時計算**。攻撃・反撃ダメージを両者同時に算出して適用する（両方消滅もありうる）
  - **ダメージ計算:** `(base_damage * display_hp / 10 + random(0..=10))` を防衛側のHPから減算。攻撃側には乗算前の base_damage に 5% のアドバンテージを加算する（ランダム値は対象外）
- 射程チェック：マップのトポロジー（4マス/6マス）に応じたマンハッタン距離で攻撃可否を判定
- 弾薬消費：使用した武器の弾薬を1消費。反撃不能時は弾薬を消費しない
- 撃破されたユニット（hp == 0）はその後の反撃を行わない

## Impact
- Affected specs: `unit-attack`（新規）、`game-state`（MODIFIED: attack メソッド追加）、`unit-roster`（MODIFIED: has_moved / action_completed 分離）
- Affected code: `src/domain/game_state.rs`, `src/domain/unit_roster.rs`, `src/domain/map_grid.rs`
