# island-detection Specification

## Purpose
AIがマップ上の隔絶された陸地領域（島）を自動で認識し、水上・空輸輸送が必要な攻略目標（島）を適切に特定するための戦略的マップ解析基盤を提供すること。これにより、各島に存在する中立・敵拠点を分析し、優先度の高い攻略計画の策定や輸送ミッションの割り当てを可能にします。
## Requirements
### Requirement: Island Detection
MUST: AIはマップの地形を解析し、連続した陸地を「島」として認識しなければならない。

#### Scenario: AI Initialization
- **WHEN** AIエンジンが初期化される、または地形が変更されたとき
- **THEN** Sea以外の地形をフラッドフィルで連結し、一意のIsland IDを持つIslandMapを生成する。

### Requirement: Movement-aware Strategic Connectivity
MUST: 地上部隊の交戦・侵攻要否は Island ID だけでなく、対象ユニットの移動タイプによる地形到達可能性で判定しなければならない。

#### Scenario: Ground Forces Separated by Sea or Shoal
- **WHEN** 近距離にいる地上ユニット同士が、Sea または地上移動不能な Shoal によって分断されているとき
- **THEN** AIはそれらを近距離の交戦相手として数えず、敵島への輸送侵攻を検討する。

#### Scenario: Air and Ship Engagement
- **WHEN** Air または Ship ユニットが別島の敵と交戦可能な距離にいるとき
- **THEN** 島の分離だけを理由に交戦候補から除外しない。

