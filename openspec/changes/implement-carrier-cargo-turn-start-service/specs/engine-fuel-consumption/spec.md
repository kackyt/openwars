## MODIFIED Requirements

### Requirement: 毎ターンの燃料消費

MUST: エンジンは、搭載中ではない航空ユニット（`MovementType::Air`）に対してだけ、各ラウンド開始時にマスターデータで定義された日毎燃料消費量を減算しなければならない。

#### Scenario: 搭載航空ユニットの燃料保護
- **GIVEN** 航空ユニットが `Transporting` を持ち、空母に搭載されている
- **WHEN** ラウンドが切り替わる
- **THEN** 日次燃料消費は行われない
- **AND** 燃料が0でも燃料切れ墜落により破壊されない
