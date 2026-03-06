# game-state Specification

## Purpose
TBD - created by archiving change add-core-logic. Update Purpose after archive.
## Requirements
### Requirement: ゲーム状態管理 (Game State Management)
システムは、現在のターンとアクティブなプレイヤーを追跡し、対戦（マッチ）のライフサイクルを管理しなければならない (SHALL)。

#### Scenario: マッチの初期化
- **WHEN** 2人のプレイヤーで新しいマッチが作成されたとき
- **THEN** ターン数を1に設定し、アクティブプレイヤーをPlayer 1に設定する

#### Scenario: ターンの進行
- **WHEN** アクティブプレイヤーがターンを終了したとき
- **THEN** アクティブプレイヤーを切り替えるか、ラウンドが完了した場合はターンカウンターをインクリメントする

