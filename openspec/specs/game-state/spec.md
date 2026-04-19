# game-state Specification

## Purpose
TBD - created by archiving change add-core-logic. Update Purpose after archive.
## Requirements
### Requirement: ゲーム状態管理 (Game State Management)
システムは、現在のターンとアクティブなプレイヤーを追跡し、対戦（マッチ）のライフサイクルを管理しなければならない (SHALL)。さらに、各プレイヤーの資金（予算）と、ゲームの勝利・終了判定を含まなければならない (SHALL)。

#### Scenario: ターンの進行と予算の獲得
- **WHEN** アクティブプレイヤーがターンを開始したとき
- **THEN** そのプレイヤーが占領している「都市」および「空港」の数に応じた所定の資金が、プレイヤーの予算に加算される

#### Scenario: 勝利条件の判定（首都占領）
- **WHEN** プレイヤーがアクション実行後に敵の「首都」を占領したとき
- **THEN** ゲームの勝利条件が満たされたと判定し、ゲーム状態を「終了（対戦完了）」に遷移させる

#### Scenario: 勝利条件の判定（敵軍全滅）
- **WHEN** ターン1以降において、敵プレイヤーの全てのユニットが破壊（消滅）したとき
- **THEN** ゲームの勝利条件が満たされたと判定し、ゲーム状態を「終了（対戦完了）」に遷移させる

### Requirement: イベント駆動型勝利判定の実行
勝利条件の判定は、ゲームの状態に変化をもたらすアクションが発生した直後にのみ実行されなければならない (MUST)。
- 判定トリガー: `PropertyCapturedEvent`, `UnitDestroyedEvent`, `GamePhaseChangedEvent` のいずれかが発行されたタイミング。

#### Scenario: アクション発生時の勝利判定
- **GIVEN** ゲーム実行中である
- **WHEN** 拠点の占領 (`PropertyCapturedEvent`) またはユニットの撃破 (`UnitDestroyedEvent`) が発生したとき
- **THEN** システムは即座に勝利条件の走査を実行し、決着がついているかを確認しなければならない (SHALL)

### Requirement: ターン交代時のクリーンアップ
フェーズ（ターン）が切り替わる際、システムは勢力の所属ユニットおよび移動中の一時データを適切にリセットしなければならない (MUST)。

#### Scenario: 移動・行動完了フラグのリセット
- **GIVEN** 前のターンで「行動不能（暗調）」状態だった自軍ユニットが存在する
- **WHEN** 自軍の次のターンが開始されたとき
- **THEN** 当該ユニットの `ActionCompleted` および `HasMoved` フラグが解除され、マップ上で通常表示（行動可能）にならなければならない (SHALL)

#### Scenario: PendingMove リソースのクリア
- **GIVEN** `PendingMove` にユニットの移動前データが保持されている
- **WHEN** ターンが終了し、次のフェーズへ移行したとき
- **THEN** `PendingMove` が空（None）にクリアされなければならない (SHALL)

