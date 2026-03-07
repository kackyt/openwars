# 攻撃アクション (Unit Attack)

## ADDED Requirements

### Requirement: 直接攻撃と間接攻撃の区別
The system SHALL distinguish attack types based on weapon range. A weapon with `min_range == 1` is a direct attack weapon; one with `min_range > 1` is an indirect attack weapon. A single unit may possess weapons of both types.
ゲームシステムは、`min_range == 1` の武器を「直接攻撃武器」、`min_range > 1` の武器を「間接攻撃武器」として区別しなければならない。一つのユニットが直接・間接両方の武器を持つことができる。

#### Scenario: 直接攻撃武器による攻撃
- **WHEN** 使用された武器の `min_range == 1` で、対象が射程内の敵ユニットである
- **THEN** 直接攻撃として処理され、攻撃後に防衛側の反撃判定が行われる

#### Scenario: 間接攻撃武器による攻撃
- **WHEN** 使用された武器の `min_range > 1` で、対象が `min_range` 以上 `max_range` 以下の距離にいる敵ユニットである
- **THEN** 間接攻撃として処理され、防衛側の反撃は発生しない

### Requirement: 攻撃可能範囲の判定
The system SHALL validate attack range using Manhattan distance based on map topology. An attack is valid only if the distance is between `min_range` and `max_range` inclusive.
システムは攻撃者と対象の距離をマップのトポロジーに基づいたマンハッタン距離で計算しなければならない。距離が `min_range` 以上 `max_range` 以下の場合のみ攻撃可能とする。

#### Scenario: 射程内攻撃の許可
- **WHEN** 攻撃者と対象のマンハッタン距離が選択武器の `min_range` 以上 `max_range` 以下であり、対象が敵ユニットである
- **THEN** 攻撃は有効とみなされる

#### Scenario: 射程外への攻撃の拒否
- **WHEN** 攻撃者と対象の距離が `max_range` を超えるか `min_range` 未満の場合
- **THEN** 攻撃は無効（エラー）となる

### Requirement: 武器の自動選択
The system SHALL automatically select a weapon: primary weapon (ammo1) takes priority; if it is out of ammo or deals 0 damage to the target, the secondary weapon (ammo2) SHALL be used instead. If neither weapon can deal damage, the attack MUST fail.
主武器（ammo1）を優先し、弾切れまたは対象へのダメージが0の場合に副武器（ammo2）を使用する。両武器とも使用不可の場合は攻撃が失敗しなければならない。

#### Scenario: 主武器を使用する場合
- **WHEN** 攻撃者の `ammo1 > 0` かつ対象ユニットに対して1以上のダメージを与えられる
- **THEN** 主武器を使用して攻撃し、`ammo1` を1減らす

#### Scenario: 副武器へのフォールバック
- **WHEN** 主武器が弾切れ（`ammo1 == 0`）またはダメージが0の場合、かつ `ammo2 > 0` でダメージを与えられる
- **THEN** 副武器を使用して攻撃し、`ammo2` を1減らす

#### Scenario: 両武器とも使用不可の場合
- **WHEN** 両方の武器が弾切れまたはダメージ0である
- **THEN** 攻撃は失敗（エラー）となる

### Requirement: ダメージ計算
The system SHALL calculate attack damage as: `base_damage * display_hp / 10 + random(0..=10)`. The attacker SHALL receive a 5% advantage applied to `base_damage` before multiplication (the random value is not affected by the advantage).
ダメージは `base_damage * display_hp / 10 + random(0..=10)` で計算される。攻撃側は乗算前の `base_damage` に +5% のアドバンテージが適用される（ランダム値には影響しない）。

#### Scenario: 攻撃側のダメージ計算
- **WHEN** 攻撃者の表示HPが 8、基本ダメージが 55 の場合
- **THEN** 攻撃側の適用ダメージ = `floor(55 * 1.05 * 8 / 10) + random(0..=10)` となる

#### Scenario: 反撃側のダメージ計算
- **WHEN** 反撃側の表示HPが 6、基本ダメージが 40 の場合
- **THEN** 反撃側ダメージ = `floor(40 * 6 / 10) + random(0..=10)`（アドバンテージなし）となる

#### Scenario: 撃破の発生
- **WHEN** ダメージ適用後に防衛側の hp が 0 以下になる
- **THEN** 防衛側ユニットは破壊（hp = 0）として扱われ、勝利条件の再チェックが行われる

### Requirement: 直接攻撃後の反撃（同時ダメージ計算）
The system SHALL resolve direct attack damage simultaneously. Both attacker and defender damage SHALL be calculated before either is applied, so mutual destruction is possible. Counter-attack damage SHALL only be computed if the defender is alive before the attack and has a weapon capable of dealing damage to the attacker.
直接攻撃（direct）の場合、攻撃・反撃のダメージを同時に算出してから両者に適用しなければならない（MUST）。反撃は攻撃前のHPで可否を判断し、両方消滅もありうる。防衛側に攻撃者へのダメージが0になる武器しかない場合は反撃ダメージを計算せず弾薬も消費しない。

#### Scenario: 反撃の発生（同時解決）
- **GIVEN** 直接攻撃を受けたユニットが攻撃処理前に存命（`hp > 0`）で、攻撃者へのダメージが1以上の武器を持つ
- **WHEN** 攻撃ダメージが計算される
- **THEN** 反撃ダメージも同時に計算され、攻撃ダメージと反撃ダメージが同時に適用される

#### Scenario: 両者同時消滅
- **WHEN** 攻撃ダメージで防衛側が撃破され、かつ反撃ダメージで攻撃側も撃破される
- **THEN** 両ユニットとも hp = 0 となり、勝利条件の再チェックが行われる

#### Scenario: ダメージ不可能による反撃なし
- **WHEN** 直接攻撃を受けたユニットの全武器で攻撃者へのダメージが0となる、または弾切れの場合
- **THEN** 反撃ダメージは計算されず、弾薬も消費しない

### Requirement: 間接攻撃の制約
Indirect attack units (`min_range > 1`) MUST NOT attack after moving in the same turn. Counter-attacks SHALL NOT occur against indirect attacks.
間接攻撃武器による攻撃は、同ターンに移動済みの場合に行えない。また、間接攻撃に対する反撃は発生しない。

#### Scenario: 移動後の間接攻撃禁止
- **WHEN** ユニットが当該ターンに移動済み（`has_moved == true`）の状態で間接攻撃（`min_range > 1`）を試みる
- **THEN** 攻撃は無効（エラー）となる

#### Scenario: 間接攻撃への反撃なし
- **WHEN** 間接攻撃を受けたユニットが存命であり使用可能な武器を持つ
- **THEN** 反撃は発生しない

### Requirement: 行動フラグの分離
Each unit MUST have a separate `has_moved` flag for movement and `action_completed` flag for attack tracking. Both flags SHALL be reset to `false` when the owning player's turn begins.
ユニットは「移動済み」フラグ（`has_moved`）と「攻撃済み」フラグ（`action_completed`）を個別に持たなければならない（MUST）。ターン開始時にはそのプレイヤーが所有するすべてのユニットのフラグを `false` にリセットしなければならない。

#### Scenario: 移動のみ行った場合のフラグ状態
- **WHEN** ユニットが移動を行い、まだ攻撃していない
- **THEN** `has_moved == true`、`action_completed == false` となる

#### Scenario: 攻撃後の行動完了フラグ
- **WHEN** ユニットが攻撃を行った後
- **THEN** `action_completed == true` となり、その後の攻撃は行えない

#### Scenario: ターン開始時のフラグリセット
- **WHEN** アクティブプレイヤーの番になった（`advance_turn` によるターン更新）
- **THEN** そのプレイヤーが所有するすべてのユニットの `has_moved` と `action_completed` が `false` にリセットされる
