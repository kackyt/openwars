## ADDED Requirements

### Requirement: SquadAssignmentPlan をビームの単位とする
SHALL: ビーム探索エンジンは、全 Squad の目標割り当ての組み合わせ（`SquadAssignmentPlan`）をビームの 1 状態として扱わなければならない。`SquadAssignmentPlan` は `Vec<(SquadId, SquadTarget)>` として表現される。

#### Scenario: 2 つの Squad があり目標が 3 候補ある場合の展開
- **WHEN** S1 と S2 の 2 Squad、目標候補 T1/T2/T3 がある
- **THEN** ビームは最大 3 × 3 = 9 通りの SquadAssignmentPlan を展開し、上位 N プランに絞る

### Requirement: ロールアウト評価による暫定スコア算出
SHALL: インクリメンタル展開の途中で未割り当て Squad が残る場合、それらを貪欲法（最適スコアのターゲットに割り当て）で補完した完成プランを生成し、`evaluate_board` で暫定スコアを算出しなければならない。ビームの絞り込みはこの暫定スコアで行う。

#### Scenario: S1 のみ割り当て済みで S2 が未割り当ての暫定評価
- **WHEN** S1 → T1 割り当て済み、S2 未割り当ての状態でビームを絞り込む
- **THEN** S2 を貪欲法で T2 に仮割り当てした完成プラン {S1→T1, S2→T2} を評価し暫定スコアを算出する

### Requirement: 完成プランの最終評価と実行
SHALL: 全 Squad が割り当てられた完成プランに対して `AiSimulationState` を用いたシミュレーション評価を実行し、最終スコアを算出しなければならない。最も高いスコアの SquadAssignmentPlan を選択し、各 Squad がミッションフェーズに従って実行する。

#### Scenario: 集中プランより分散プランが高スコアになる場合
- **WHEN** {S1→クラスターA, S2→クラスターA}（集中）と {S1→クラスターA, S2→首都防衛}（分散）を評価する
- **THEN** 首都が脅かされている状況では分散プランの方が高スコアになり採択される

### Requirement: ビーム幅の設定
SHALL: ビーム幅 N は設定可能な定数として定義しなければならない。デフォルト値は 5 とし、MCP ツールによる評価実験を経てチューニング可能にする。

#### Scenario: ビーム幅を超えた候補の打ち切り
- **WHEN** ビーム展開で 10 個の SquadAssignmentPlan 候補が生成された（ビーム幅 = 5）
- **THEN** 暫定スコア上位 5 プランのみが保持され、残り 5 は破棄される

### Requirement: SoloFallback ユニットの貪欲法実行
SHALL: ビーム探索が完了した後、Squad に属していない SoloFallback ユニットは従来の `decide_ai_action` 貪欲法で行動しなければならない。SoloFallback ユニットは Squad 実行後に行動させる。

#### Scenario: SoloFallback ユニットが Squad 実行後に行動する場合
- **WHEN** Squad 実行が完了し、SoloFallback 状態のユニットが残っている
- **THEN** SoloFallback ユニットは `decide_ai_action` で個別に最善手を決定し実行する
