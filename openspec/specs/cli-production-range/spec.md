# cli-production-range Specification

## Purpose
首都からの距離に基づいた生産制限を視覚的にユーザーに提示し、不正な生産を防止するための仕様を定義します。

## Requirements

### Requirement: 生産可能範囲の可視化 (Production Range Visualization)
システムは、生産施設がプレイヤーの首都から一定の範囲（距離 3 以内）を超えている場合、警告を表示しなければならない (MUST)。

#### Scenario: 範囲外の施設での警告表示 (Production out of range shows warning)
- **WHEN** ユーザーが、自軍の首都からマンハッタン距離で 4 以上離れた場所にある工場などの拠点でアクションメニューを開いた場合
- **THEN** システムは、距離が遠いため生産不可能であることを示す警告を表示し、「生産」コマンドをメニューから除外、または無効化しなければならない (SHALL)。
