## ADDED Requirements

### Requirement: Production Range Visualization
MUST: システムは、生産施設がプレイヤーの首都から範囲外にある場合、警告を表示する。

#### Scenario: Production out of range shows warning
- **WHEN** ユーザーが、首都から距離が4以上離れた工場のプロパティでアクションメニューを開いた場合
- **THEN** SHALL: システムは、距離が遠すぎるため生産不可能であることを示す警告を表示し、「生産」オプションを無効化する。
