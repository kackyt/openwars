# island-detection Specification

## Purpose
TBD - created by archiving change ai-strategic-engine-phase1. Update Purpose after archive.
## Requirements
### Requirement: Island Detection
MUST: AIはマップの地形を解析し、連続した陸地を「島」として認識しなければならない。

#### Scenario: AI Initialization
- **WHEN** AIエンジンが初期化される、または地形が変更されたとき
- **THEN** Sea以外の地形をフラッドフィルで連結し、一意のIsland IDを持つIslandMapを生成する。

