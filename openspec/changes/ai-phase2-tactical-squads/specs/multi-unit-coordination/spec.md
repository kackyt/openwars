## MODIFIED Requirements

### Requirement: Multi-Unit Coordination
MUST: AIは Squad システムを通じて複数ユニットを協調させなければならない。SquadPlanner が GamePhase と ClusterMap に基づいて Attack / Capture / Defense / Transport の各 Squad を形成し、BeamSearch が全 Squad の目標割り当ての最適組み合わせ（「集中か分散か」の判断を含む）を選択する。TransportMission は TransportSquad として Squad システムに統合される。

#### Scenario: 複数の部隊が異なる目標に分散して担当する場合
- **WHEN** ビーム探索が「全部隊を同一クラスターに集中させるプラン」と「2 部隊がそれぞれ異なるクラスターを担当するプラン」を評価する
- **THEN** 盤面評価スコアが高い方のプランが採択される（首都近傍の脅威が高ければ分散、前線突破が有効なら集中）

#### Scenario: 輸送ミッションが Squad システムで管理される場合
- **WHEN** 輸送ヘリと歩兵の輸送ミッションが SquadManager.update() で再編成チェックされる
- **THEN** 従来の TransportMissionManager と同等の Pickup → Transit → Drop → Return フェーズ管理が Squad システムで実行される
