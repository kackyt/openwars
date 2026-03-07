# Tasks: implement-attack

- [x] 1. `src/domain/unit_roster.rs` の `Unit` に `has_moved: bool` フィールドを追加し、`action_completed` を「攻撃済み」フラグとして再定義する。`move_unit` は `has_moved` のみセットし、`action_completed` はセットしないよう修正する。
- [x] 2. `src/domain/game_state.rs` に `attack` メソッドを追加する。
    - 攻撃者と防衛者のユニットインデックスを受け取る
    - 射程チェック：マップのトポロジーに合わせた距離計算（4方向ならマンハッタン距離）
    - 間接攻撃（`min_range > 1` かつ `max_range > 1`）の場合、`has_moved == true` なら攻撃不可
    - 武器選択ロジック（主武器 → 副武器にフォールバック）を実装
    - ダメージ計算： `DamageChart` から基本ダメージを取得し、`base_damage * display_hp / 10 + random(0..=10)` で算出。攻撃側の乗算前 base_damage には 5% アドバンテージを加算する（random 値には影響しない）
    - 撃破判定後に `check_win_conditions` を呼ぶ
- [x] 3. 直接攻撃の場合、攻撃・反撃のダメージを同時計算して適用する（防衛側が存命かつ攻撃者へのダメージが1以上の武器を持つ場合のみ反撃ダメージを計算）。反撃進行かどうかは攻撃前のHPで判断する。両方消滅もありうる。
- [x] 4. 単体テストを追加する
    - 直接攻撃：HP 按分ダメージ・反撃・弾薬消費・弾切れ反撃不可
    - 間接攻撃：一方的で反撃なし・移動後攻撃不可
    - 射程外攻撃のエラー
    - 撃破時の勝利条件判定
    - 主武器→副武器フォールバック
