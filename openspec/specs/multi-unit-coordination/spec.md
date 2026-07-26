# multi-unit-coordination Specification

## Purpose
TBD - created by archiving change ai-strategic-engine-phase2. Update Purpose after archive.
## Requirements
### Requirement: Multi-Unit Coordination
MUST: AIは1つの目標に対して、適切な数のユニットを連携させて割り当てなければならない。

#### Scenario: Coordinated Assignment
- **WHEN** AIプランナーが目標攻略のためのミッションを生成するとき
- **THEN** 対象の島・拠点の規模に応じて必要な占領要員数を計算し、輸送容量の範囲で占領要員と戦闘要員を同じ侵攻波へ割り当てる。輸送待ちユニットと輸送役は、双方が到達可能な合法な Pickup 位置へ移動する。

