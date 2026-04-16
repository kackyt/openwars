## ADDED Requirements

### Requirement: 毎ターンの燃料消費 (Daily Fuel Consumption)
MUST: エンジンは、各ターンの開始時に、すべての航空ユニット（`MovementType::Air`）に対して、マスターデータで定義された「日毎燃料消費量」分の燃料を減算しなければならない。

#### Scenario: 空中待機中の燃料消費
- **WHEN** 航空ユニットがターン終了時に空港（`Terrain::Airport`）以外のタイルに滞在している場合
- **THEN** 次の自ターン開始までに、当該ユニットの `Fuel` コンポーネントから `daily_fuel_consumption` 分の値が差し引かれること。

#### Scenario: 空港での燃料補給による相殺
- **WHEN** 航空ユニットが空港（`Terrain::Airport`）に滞在している場合
- **THEN** 日次消費は行われず（または補給によって相殺され）、燃料切れによる墜落判定も行われないこと。

### Requirement: 燃料切れによる墜落 (Crash due to Empty Fuel)
MUST: エンジンは、航空ユニットの燃料が 0 になった状態で、かつそのユニットが補給可能な施設（空港等）にいない場合、当該ユニットを即座に破壊（HPを0に設定）しなければならない。

#### Scenario: 燃料切れ墜落の発生
- **WHEN** 航空ユニットの燃料が、日次消費によって 0 になったとき
- **THEN** 当該ユニットの `Health` コンポーネントの `current` 値が 0 に更新されること。
