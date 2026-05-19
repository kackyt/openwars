# island-detection Specification

## Purpose
AIがマップ上の隔絶された陸地領域（島）を自動で認識し、水上・空輸輸送が必要な攻略目標（島）を適切に特定するための戦略的マップ解析基盤を提供すること。これにより、各島に存在する中立・敵拠点を分析し、優先度の高い攻略計画の策定や輸送ミッションの割り当てを可能にします。
## Requirements
### Requirement: Island Detection
MUST: AIはマップの地形を解析し、連続した陸地を「島」として認識しなければならない。

#### Scenario: AI Initialization
- **WHEN** AIエンジンが初期化される、または地形が変更されたとき
- **THEN** Sea以外の地形をフラッドフィルで連結し、一意のIsland IDを持つIslandMapを生成する。

