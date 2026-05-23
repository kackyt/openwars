## ADDED Requirements

### Requirement: 敵ユニットのターン数距離クラスタリング
SHALL: システムは、`TurnDistance` を用いて敵ユニット群をクラスターに分類できなければならない。2体の敵ユニット間の相互到達ターン数が `CLUSTER_RADIUS_TURNS`（デフォルト 2）以下の場合、同一クラスターとみなす。

#### Scenario: 近接した2体の敵ユニットが同一クラスターになる場合
- **WHEN** 敵 Tank_A と敵 Tank_B が相互に 1 ターンで到達可能な位置にいる
- **THEN** 両者は同一の `AttackCluster` に属する

#### Scenario: 海を挟んで存在する地上ユニットが別クラスターになる場合
- **WHEN** 地上ユニット 2 体が海を挟んだ別島にいる（TurnDistance = ∞）
- **THEN** 両者は異なるクラスターに属する

### Requirement: クラスターの脅威レベル算出
SHALL: 各 `AttackCluster` は、自軍拠点（首都・工場・都市）への最短到達ターン数を「脅威レベル」として保持しなければならない。脅威レベルが低いクラスターほど優先して対処すべきターゲットとして評価される。

#### Scenario: 首都に 2 ターンで到達可能な敵クラスターの脅威レベル
- **WHEN** クラスター内の最速ユニットが自軍首都へ 2 ターンで到達可能
- **THEN** クラスターの脅威レベル = 2

### Requirement: クラスターへの自軍到達ターン数算出
SHALL: 各 `AttackCluster` は、指定した自軍ユニット（または部隊）から当該クラスターへの最短到達ターン数を計算できなければならない。`min_turns_to_engage(from, movement_type, max_movement)` として実装する。

#### Scenario: 自軍部隊からのクラスター接近難易度
- **WHEN** 自軍部隊が 3 ターンでクラスターの最近接ユニットに到達できる
- **THEN** `min_turns_to_engage` = 3 を返す
