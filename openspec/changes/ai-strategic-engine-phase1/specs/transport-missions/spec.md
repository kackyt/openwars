## ADDED Requirements

### Requirement: Transport Mission Definition
AIは複数ターンにまたがる輸送行動を「ミッション」として管理し、既存の貪欲アルゴリズムよりも優先して実行しなければならない。

#### Scenario: Mission Execution
- **WHEN** AIの行動決定ループ (`decide_ai_action`) が開始したとき
- **THEN** まず全てのユニットについて、自身に割り当てられたミッションがあるか確認し、ミッションが存在する場合はそのフェーズ（Pickup, Transit, Drop, Return）に従った行動を優先的に実行する。ミッションを持たないユニットのみが貪欲法による行動決定を行う。
