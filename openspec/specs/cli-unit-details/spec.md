# cli-unit-details Specification

## Purpose
マップ上のユニットにカーソルを合わせた際、そのユニットの動的なステータス（HP、燃料、残弾など）を詳細に表示するための仕様を定義します。

## Requirements

### Requirement: ユニット詳細インスペクター (Unit Inspector Overlay)
システムは、ユーザーがマップ上のユニットにカーソルを合わせたとき、そのユニットの詳細なステータス（HP、弾薬数、燃料量など）をオーバーレイ表示しなければならない (MUST)。

#### Scenario: ユニット情報のホバー表示 (Hovering over a unit displays details)
- **WHEN** ユーザーが、マップ上に配置されたユニットの上にカーソルを置いた場合
- **THEN** システムは、当該ユニットの現在の HP、燃料、および主武器・副武器の弾薬数を記載したインスペクション・パネルを表示しなければならない (SHALL)。
