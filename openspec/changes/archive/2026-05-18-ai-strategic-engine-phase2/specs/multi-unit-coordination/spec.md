## ADDED Requirements

### Requirement: Multi-Unit Coordination
MUST: AIは1つの目標に対して、適切な数のユニットを連携させて割り当てなければならない。

#### Scenario: Coordinated Assignment
- **WHEN** AIプランナーが目標攻略のためのミッションを生成するとき
- **THEN** 対象の島・拠点の規模に応じて必要な歩兵部隊数を計算し、その数だけ輸送機と歩兵のペアに対してTransportMissionを割り当てる。また、輸送待ちの歩兵は海岸線に向かって移動し、効率よく搭乗できるようにする。
