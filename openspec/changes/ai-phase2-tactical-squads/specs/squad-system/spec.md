## ADDED Requirements

### Requirement: Squad 構造体の定義
SHALL: システムは、部隊（Squad）を「ミッション種別・メンバーエンティティリスト・戦術目標・最小メンバー数・フェーズ」を保持する構造体として定義しなければならない。ミッション種別は AttackMission / CaptureMission / DefenseMission / TransportMission の 4 種とする。

#### Scenario: AttackMission の Squad が生成される場合
- **WHEN** SquadPlanner が 2 体の戦闘ユニットを Attack 目標クラスターへ割り当てる
- **THEN** squad.mission = AttackMission(AttackPhase::Converge), squad.members = [A, B], squad.target = AttackCluster として生成される

### Requirement: SquadManager による毎ターン再編成
SHALL: SquadManager は毎ターン開始時に、(1) ミッション完了チェック・解散、(2) ミッション破綻チェック（メンバー数 < min_members → 解散）、(3) ターゲット更新・メンバー入れ替えを実施しなければならない。

#### Scenario: AttackMission のターゲットクラスターが全滅した場合
- **WHEN** Squad のターゲットクラスター内の全敵ユニットが撃破済み
- **THEN** Squad は解散し、メンバーは SoloFallback または次の SquadPlanner 割り当てに移行する

#### Scenario: Squad のメンバーが最小数を下回った場合
- **WHEN** 戦闘中に Squad のメンバーが min_members 未満になる
- **THEN** Squad は解散し、残存メンバーは SoloFallback 状態に遷移する

### Requirement: SquadPlanner のルールベース部隊形成
SHALL: SquadPlanner は `GamePhase` と `ClusterMap` を入力として受け取り、以下のルールに従って Squad を形成しなければならない:
1. `GamePhase::Defense` → DefenseMission を最優先で形成（首都 5 ターン圏内の敵クラスターが対象）
2. `GamePhase::Expansion` → CaptureMission を優先（自軍の島の未占領拠点が対象）、次に AttackMission
3. `GamePhase::Contested / Assault` → AttackMission を優先（脅威レベルが低いクラスターを優先ターゲット）
ユニット割り当ては「ミッション適合スコア」が高い順に行い、既に他 Squad に割り当て済みのユニットは -500 のペナルティを与える。

#### Scenario: Expansion フェーズで未占領拠点が 2 つある場合
- **WHEN** GamePhase = Expansion、自軍の島に中立拠点が 2 つ存在し、歩兵が 2 体いる
- **THEN** 2 つの CaptureMission が形成され、各歩兵に 1 つずつ割り当てられる

#### Scenario: Defense フェーズで首都に敵クラスターが接近している場合
- **WHEN** GamePhase = Defense、首都へ 3 ターンで到達可能な敵クラスターが存在する
- **THEN** DefenseMission が最優先で形成され、首都近傍の戦闘ユニットが割り当てられる

### Requirement: SoloFallback メカニズム
SHALL: ユニットは HP < 60 または主武装弾薬 = 0 の場合、Squad 割り当てから除外され SoloFallback 状態に遷移しなければならない。SoloFallback 状態では既存の貪欲法で行動し、評価スコアに「最寄り受入可能 Squad への接近ボーナス」が加算される。

#### Scenario: 損傷ユニットが SoloFallback に遷移する場合
- **WHEN** Squad メンバーの HP が 50 に下がった
- **THEN** そのユニットは Squad から除外され、次の SquadManager.update() で SoloFallback 状態に設定される

#### Scenario: 回復したユニットが Squad 復帰候補になる場合
- **WHEN** SoloFallback ユニットの HP ≥ 70 かつ弾薬補充済み かつ 最寄り Squad に空きがある
- **THEN** 次の SquadPlanner サイクルでそのユニットは Squad 割り当て候補として再評価される

### Requirement: TransportMission の Squad への統合
SHALL: 既存の `TransportMission` は `Squad::Transport(TransportPhase)` として内包され、`SquadManager` によって管理されなければならない。従来の `TransportMissionManager` は廃止し、`SquadManager` に一本化する。既存のフェーズ遷移ロジック（Pickup → Transit → Drop → Return）はそのまま維持する。

#### Scenario: 既存の TransportMission が Squad として管理される場合
- **WHEN** AI がターン開始時に SquadManager.update() を呼ぶ
- **THEN** 輸送中の Squad (TransportMission) もミッション完了・破綻チェックの対象となる
