# transport-missions Specification

## Purpose
AIが海や進入不可能な地形で隔てられた遠隔地・島へのユニット輸送を、複数ターンにまたがる体系的な「ミッション」（回収、移送、降車、帰還の各フェーズ）として定義・管理すること。これにより、単ーターンのみを考慮する貪欲法（Greedy）の限界を克服し、戦略的に不可欠な長距離ユニット展開や島嶼部の占領行動を優先的かつ確実に実行できるようにします。
## Requirements
### Requirement: Transport Mission Definition
MUST: AIは複数ターンにまたがる輸送行動を「ミッション」として管理し、既存の貪欲アルゴリズムよりも優先して実行しなければならない。

#### Scenario: Mission Execution
- **WHEN** AIの行動決定ループ (`decide_ai_action`) が開始したとき
- **THEN** まず全てのユニットについて、自身に割り当てられたミッションがあるか確認し、ミッションが存在する場合はそのフェーズ（Pickup, Transit, Drop, Return）に従った行動を優先的に実行する。ミッションを持たないユニットのみが貪欲法による行動決定を行う。

