# 仕様: ユニット攻撃とダメージ計算 (Unit Attack & Damage Calculation)

## 概要
ユニット間の戦闘、および地形効果を加味したダメージ計算のアルゴリズムを定義します。

## MODIFIED Requirements

### Requirement: 地形防御ボーナスの適用 (Applied Terrain Defense)
ダメージ計算には、マスターデータ（`landscape.csv`）から取得した防衛側地形の `defense_bonus` を使用しなければならない (MUST)。従来のハードコードされた `Terrain::defense_stars()` は廃止されなければならない (MUST)。

#### Scenario: 山にいるユニットへのダメージ計算
- **GIVEN** 防衛側のユニットが「山（防御ボーナス: 40）」にいる
- **WHEN** 攻撃側からダメージ計算が行われるとき
- **THEN** 地形防御値 40 が計算式の分母要素として正しく適用される。

### Requirement: 除算ベースのダメージ計算式 (Division-Based Damage Formula)
ダメージ計算は以下の「除算方式」を用い、HPの減少による威力低下、地形による軽減、および攻撃側の優位性を同時に再現しなければならない (SHALL)。
  - **攻撃側**: `damage = (base_damage * attacker_hp + 105) / (100 + terrain_defense_bonus) + luck`
  - **反撃側**: `damage = (base_damage * defender_hp + 100) / (100 + terrain_defense_bonus) + luck`
  - `luck` は 0 から 10 の範囲の整数乱数とし、ダメージ結果に加算されなければならない (MUST)。

#### Scenario: 平地での標準的な戦闘ダメージ（攻撃側）
- **GIVEN** 基本威力 55%, 攻撃側HP: 100, 防衛側が平地（防御 0）にいる, 乱数が 2
- **WHEN** 攻撃側からダメージ計算が行われたとき
- **THEN** 計算結果が (55 * 100 + 105) / 100 + 2 = 56 + 2 = 58 となり、最終的なHP減少量として適用される。

#### Scenario: 平地での標準的な戦闘ダメージ（反撃側）
- **GIVEN** 基本威力 55%, 反撃側HP: 100, 攻撃側が平地（防御 0）にいる, 乱数が 2
- **WHEN** 反撃側からダメージ計算が行われたとき
- **THEN** 計算結果が (55 * 100 + 100) / 100 + 2 = 56 + 2 = 58 となり、最終的なHP減少量として適用される。
- **NOTE** このケースでは端数の関係で攻撃側と同じ結果になるが、威力が低い場合や防御が高い場合に攻撃側が有利に働く。

### Requirement: 反撃の自動発生 (Automatic Counter-attack)
被攻撃ユニットが生存しており、かつ攻撃側が相手の反撃可能射程内にいる場合、被攻撃側は反撃（Counter-attack）を実行しなければならない (MUST)。

#### Scenario: 射程1同士の近接戦闘
- **GIVEN** 隣接する2つのユニットA, B（射程1）がいる
- **WHEN** ユニットAがBを攻撃したとき
- **THEN** Aの攻撃完了後にBが生存していれば、BからAへの反撃アクションが自動的に連鎖する。
