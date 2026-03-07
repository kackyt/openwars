# 補給アクション (Unit Supply)

## ADDED Requirements

### Requirement: 補給輸送車による隣接ユニット補給
The SupplyTruck unit SHALL be able to supply adjacent friendly units (Manhattan distance == 1), restoring their fuel and ammo to maximum values. The supply action SHALL be available after moving and MUST mark the supplier's `action_completed` as true.
補給輸送車は、隣接マス（マンハッタン距離 = 1）にいる味方ユニットの燃料・弾薬を最大値まで回復させる補給アクションを実行できなければならない。補給は移動後も実行可能で、実行後は `action_completed = true` となる。

#### Scenario: 隣接ユニットへの単体補給
- **GIVEN** 補給輸送車が行動完了していない（`action_completed == false`）
- **AND** 対象味方ユニットが補給輸送車から距離 = 1 に存在する
- **WHEN** `supply_unit(supplier_index, target_index)` を呼ぶ
- **THEN** 対象ユニットの `fuel`、`ammo1`、`ammo2` が最大値まで回復する
- **AND** 補給者の `action_completed == true` となる

#### Scenario: 補給輸送車が移動後に補給する
- **WHEN** 補給輸送車の `has_moved == true` の状態で `supply_unit` を呼ぶ
- **THEN** 補給は正常に実行される（移動後でも補給可能）

#### Scenario: 射程外ユニットへの補給エラー
- **WHEN** 対象ユニットが補給輸送車から距離 > 1 の位置にいる
- **THEN** 補給は無効（エラー）となる

### Requirement: 補給輸送車による全自動補給
The SupplyTruck SHALL support a batch supply mode that automatically supplies all adjacent friendly units in a single action.
補給輸送車は、隣接するすべての味方ユニットに一括で補給する全自動補給モードをサポートしなければならない（MUST）。

#### Scenario: 全隣接ユニットへの一括補給
- **GIVEN** 補給輸送車が行動完了していない
- **AND** 隣接マスに複数の味方ユニットが存在する
- **WHEN** `supply_all_adjacent(supplier_index)` を呼ぶ
- **THEN** すべての隣接味方ユニットの燃料・弾薬が最大値まで回復する
- **AND** 補給者の `action_completed == true` となる

#### Scenario: 隣接に補給対象なし
- **WHEN** `supply_all_adjacent` を呼んだとき隣接に味方ユニットがいない
- **THEN** 補給者の `action_completed == true` となる（空振りは許可）

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
- **GIVEN** 味方ユニットが拠点マスにいる
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
