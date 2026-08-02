# unit-supply Specification

## Purpose
補給輸送車、拠点、空母による補給の対象・状態変更・コストを定義する。

## Requirements
### Requirement: 補給輸送車による隣接ユニットへの単体補給
The SupplyTruck SHALL allow the player to select exactly one eligible adjacent friendly unit and restore that unit's fuel and ammo to maximum values. Adjacency SHALL use the active map topology. The supply action SHALL be available after moving and MUST mark only the supplier's `action_completed` as true.
補給輸送車は、マップのトポロジーに基づいて隣接する補給可能な味方ユニットを1部隊だけ選択し、その燃料・弾薬を最大値まで回復できなければならない。補給は移動後も実行可能で、実行後は補給者だけが `action_completed = true` となる。

補給可能な対象は、未行動かつ生存中で、搭載されていない全地上ユニットおよび軽戦闘機（`Fighter`）とする。戦艦、空母、輸送船および重戦闘機、爆撃機、戦闘ヘリ、輸送ヘリは補給対象外とする。

#### Scenario: 隣接ユニットへの単体補給
- **GIVEN** 補給輸送車が行動完了していない（`action_completed == false`）
- **AND** 補給輸送車が移動後の位置を確定している
- **AND** その位置から距離 = 1 に、未行動・生存中の補給対象となる味方ユニットが存在する
- **WHEN** プレイヤーが対象ユニットを1部隊選択して `supply_unit(supplier_index, target_index)` を呼ぶ
- **THEN** 選択対象の `fuel`、`ammo1`、`ammo2` が最大値まで回復する
- **AND** 選択されなかった隣接ユニットの状態は変化しない
- **AND** 補給者の `action_completed == true` となる

#### Scenario: 補給対象の状態を維持する
- **GIVEN** 未行動の補給対象ユニットの HP と移動状態が記録されている
- **WHEN** 補給輸送車がそのユニットへ補給する
- **THEN** 対象ユニットの HP は変化しない
- **AND** 対象ユニットの `action_completed == false` のままである
- **AND** 対象ユニットの移動状態は変化しない

#### Scenario: 補給対象外ユニットまたは行動済みユニットへの補給エラー
- **GIVEN** 対象が行動済み、撃破済み、搭載中、敵軍、非隣接、または補給対象外の種別である
- **WHEN** `supply_unit(supplier_index, target_index)` を呼ぶ
- **THEN** 補給は無効となり、補給者・対象・移動保留状態は変化しない

### Requirement: 拠点によるターン開始時自動補給
Friendly units standing on a player-owned property SHALL be automatically resupplied at the start of that player's turn. The cost SHALL be deducted from the player's funds: 15G per ammo unit restored, 5G per fuel unit restored. If the player has insufficient funds, no resupply SHALL occur.
自国の拠点（首都・都市・工場・空港・港）に乗っている味方ユニットは、そのプレイヤーのターン開始時に自動補給されなければならない（MUST）。補給コストは弾薬 1 につき 15G、燃料 1 につき 5G として差し引く。資金不足の場合は補給しない。

#### Scenario: ターン開始時の拠点補給
- **GIVEN** 味方ユニットが自国の拠点マスにいる
- **AND** プレイヤーが補給コストを賄う十分な資金を持つ
- **WHEN** `advance_turn` によりそのプレイヤーの番になった
- **THEN** ユニットの `fuel`、`ammo1`、`ammo2` が最大値まで回復する
- **AND** 補給コスト（弾薬差 × 15G + 燃料差 × 5G）がプレイヤーの資金から差し引かれる

#### Scenario: 資金不足による補給スキップ
- **GIVEN** 味方ユニットが自国の拠点マスにいる
- **AND** プレイヤーの資金が補給コストより少ない
- **WHEN** `advance_turn` によりそのプレイヤーの番になった
- **THEN** そのユニットへの補給は行われず、資金も変動しない

### Requirement: 空母による搭載航空ユニット補給
The AircraftCarrier unit SHALL supply all embarked air units when a supply action is executed, restoring their fuel and ammo to maximum values. The carrier's `action_completed` SHALL be set to true after the action.
空母は搭載している航空ユニットの燃料・弾薬を最大値まで回復させる補給アクションを実行できなければならない（MUST）。実行後は空母の `action_completed == true` となる。

#### Scenario: 空母による搭載ユニット補給
- **GIVEN** 空母が行動完了していない
- **AND** 空母に 1 機以上の航空ユニットが搭載されている
- **WHEN** 空母の補給アクションを呼ぶ
- **THEN** すべての搭載航空ユニットの燃料・弾薬が最大値まで回復する
- **AND** 空母の `action_completed == true` となる
