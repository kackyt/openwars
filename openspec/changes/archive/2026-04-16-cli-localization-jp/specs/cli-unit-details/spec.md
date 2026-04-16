## MODIFIED Requirements

### Requirement: ユニット詳細インスペクター (Unit Inspector Overlay)
MUST: システムは、ユーザーがマップ上のユニットにカーソルを合わせたとき、そのユニットの詳細なステータス（HP、弾薬数、燃料量など）を日本語のラベルでオーバーレイ表示しなければならない。

#### Scenario: ユニット情報のホバー表示 (Hovering over a unit displays details)
- **WHEN** ユーザーが、マップ上に配置されたユニットの上にカーソルを置いた場合
- **THEN** システムは、当該ユニットの現在の HP、燃料、およびマスターデータに基づいた個別の武器名と弾薬数を記載したインスペクション・パネルを日本語で表示しなければならない (SHALL)。
