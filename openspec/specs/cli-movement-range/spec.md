# cli-movement-range Specification

## Purpose
ユニット選択時に、そのユニットが移動可能な範囲を計算し、ダイナミックに可視化するための仕様を定義します。

## Requirements

### Requirement: 移動範囲の可視化 (Movement Range Visualization)
システムは、ユニットが選択された際、そのユニットの移動タイプ、地形コスト、および残り燃料に基づいて到達可能なタイルを強調表示しなければならない (MUST)。

#### Scenario: ユニット選択時のハイライト表示 (Unit selected highlights movable tiles)
- **WHEN** ユーザーが操作対象のユニットを選択した場合
- **THEN** システムは、現在のパラメータに基づいた移動可能範囲を即座に計算し、マップ上のタイル背景をハイライト表示しなければならない (SHALL)。
