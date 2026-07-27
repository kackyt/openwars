# transport-missions Specification

## Purpose
AIが海や進入不可能な地形で隔てられた遠隔地・島へのユニット輸送を、複数ターンにまたがる体系的な「ミッション」（回収、移送、降車、帰還の各フェーズ）として定義・管理すること。これにより、単ーターンのみを考慮する貪欲法（Greedy）の限界を克服し、戦略的に不可欠な長距離ユニット展開や島嶼部の占領行動を優先的かつ確実に実行できるようにします。
## Requirements
### Requirement: Transport Mission Definition
MUST: AIは複数ターンにまたがる輸送行動を「ミッション」として管理し、既存の貪欲アルゴリズムよりも優先して実行しなければならない。

#### Scenario: Mission Execution
- **WHEN** AIの行動決定ループ (`decide_ai_action`) が開始したとき
- **THEN** まず全てのユニットについて、自身に割り当てられたミッションがあるか確認し、ミッションが存在する場合はそのフェーズ（Pickup, Transit, Drop, Return）に従った行動を優先的に実行する。ミッションを持たないユニットのみが貪欲法による行動決定を行う。

### Requirement: Explicit Multi-Cargo Transport Squad
MUST: V2/V3 の輸送部隊は輸送役を明示的に保持し、輸送容量以下の順序付きカーゴ一覧を管理しなければならない。

#### Scenario: Mixed Invasion Wave
- **WHEN** 容量2以上の輸送ユニットで敵島へ侵攻するとき
- **THEN** 占領要員を優先し、搭載可能であれば戦闘要員を同じ侵攻波へ割り当てる。同じカーゴを複数の輸送部隊へ割り当てない。

### Requirement: Multi-Cargo Phase Guards
MUST: 輸送部隊の状態遷移は、指名された全カーゴと実際の CargoCapacity を基準にしなければならない。

#### Scenario: Pickup Completion
- **WHEN** 指名カーゴの一部だけが搭載済みのとき
- **THEN** Pickup を維持し、全ての有効な指名カーゴが搭載されるまで Transit へ進まない。

#### Scenario: Drop Completion
- **WHEN** 1体を降車した後も輸送ユニット内にカーゴが残っているとき
- **THEN** Drop を維持し、CargoCapacity が空になるまで Return へ進まない。

### Requirement: Legal and Safe Landing Selection
MUST: 上陸地点は既存の乗降ルールを利用して合法性を確認し、対象拠点への地上到達可能性と敵間接攻撃の期待損失を評価しなければならない。

#### Scenario: Safe Alternative Exists
- **WHEN** 対象拠点へ到達できる合法な上陸地点が複数あり、一方だけが敵間接攻撃の射程内であるとき
- **THEN** 射程外の地点を選択する。全地点が危険な場合は期待損失が最小の合法地点を選ぶ。

### Requirement: Post-Landing Handoff
MUST: 降車済みカーゴは輸送部隊のスキップ対象から除外し、通常の部隊へ引き渡さなければならない。

#### Scenario: Cargo Delivered
- **WHEN** 占領要員または戦闘要員が対象島へ降車したとき
- **THEN** 占領要員を Capture 部隊へ、戦闘要員を Attack または Defense 部隊へ割り当て、輸送部隊は残りカーゴだけを追跡する。

