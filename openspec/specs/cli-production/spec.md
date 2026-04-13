# cli-production Specification

## Purpose
拠点（工場、空港、港）におけるユニット生産メニューの表示およびフィルタリングの仕様を定義します。

## Requirements

### Requirement: 施設ごとの生産メニュー (Factory Production Menu)
システムは、利用可能な施設（工場、空港、港）の種類に応じて、生産可能なユニットをフィルタリングした動的なメニューを提供しなければならない (MUST)。

#### Scenario: 工場での生産ユニット表示 (Factory properties show correct units)
- **WHEN** ユーザーが工場拠点で生産メニューを開いた場合
- **THEN** システムは、陸上車両と歩兵ユニットのみを含むリストを表示しなければならない (SHALL)。

#### Scenario: 空港での生産ユニット表示 (Airport properties show correct units)
- **WHEN** ユーザーが空港拠点で生産メニューを開いた場合
- **THEN** システムは、航空ユニットのみを含むリストを表示しなければならない (SHALL)。

#### Scenario: 港での生産ユニット表示 (Port properties show correct units)
- **WHEN** ユーザーが港拠点で生産メニューを開いた場合
- **THEN** システムは、艦船ユニットのみを含むリストを表示しなければならない (SHALL)。
