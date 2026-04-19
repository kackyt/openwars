## ADDED Requirements

### Requirement: イベント駆動型勝利判定の実行
勝利条件の判定は、ゲームの状態に変化をもたらすアクションが発生した直後にのみ実行されなければならない (MUST)。
- 判定トリガー: `PropertyCapturedEvent`, `UnitDestroyedEvent`, `GamePhaseChangedEvent` のいずれかが発行されたタイミング。

#### Scenario: アクション発生時の勝利判定
- **GIVEN** ゲーム実行中である
- **WHEN** 拠点の占領 (`PropertyCapturedEvent`) またはユニットの撃破 (`UnitDestroyedEvent`) が発生したとき
- **THEN** システムは即座に勝利条件の走査を実行し、決着がついているかを確認しなければならない (SHALL)
