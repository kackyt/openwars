## ADDED Requirements

### Requirement: ターン数距離計算
SHALL: システムは、任意の2地点間を特定のユニット種別が移動するために要するターン数を計算できなければならない。計算は地形コストおよび移動タイプ（Air/Ship/Infantry/Tank 等）を考慮し、到達不可能な場合は `u32::MAX` を返す。

#### Scenario: 地上ユニットが山岳地帯を通過する場合
- **WHEN** 移動タイプ Infantry のユニット（移動力 3）が山岳コスト 2 のタイル 3 枚を通過する
- **THEN** TurnDistance = ceil((2+2+2)/3) = 2 ターンを返す

#### Scenario: 地上ユニットが海タイルへ到達しようとする場合
- **WHEN** 移動タイプ Infantry のユニットが Sea タイルを目標とする
- **THEN** TurnDistance = `u32::MAX`（到達不可）を返す

#### Scenario: 航空ユニットが距離 10 の目標へ向かう場合
- **WHEN** 移動タイプ Air のユニット（移動力 8）が直線距離 10 の目標へ向かう（移動コスト = 1）
- **THEN** TurnDistance = ceil(10/8) = 2 ターンを返す

### Requirement: ターン数距離のキャッシュ
SHALL: 同一ターン内で同じ（出発地、目標地、移動タイプ、移動力）の組み合わせに対する TurnDistance 計算結果はキャッシュされなければならない。再計算は行わない。

#### Scenario: 同一ターン内で同じクエリが複数回呼ばれる場合
- **WHEN** 同じ（出発地, 目標地, 移動タイプ）で TurnDistance が 2 回以上呼ばれる
- **THEN** 2 回目以降はキャッシュから返し、BFS を再実行しない
