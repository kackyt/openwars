## ADDED Requirements

### Requirement: Production Integration
AIは目標達成に必要なユニット数が不足している場合、動的に生産要求を発行しなければならない。

#### Scenario: Unit Shortage
- **WHEN** 戦略目標（島）に対して歩兵や輸送機を割り当てる際、稼働可能な待機ユニットが不足しているとき
- **THEN** 最も前線に近い自軍の工場や空港・港に対して、歩兵や輸送機の生産要求（スコア）を高め、生産フェーズで資金が許す限りそれらを生産させる。
