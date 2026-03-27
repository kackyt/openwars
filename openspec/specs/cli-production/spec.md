## ADDED Requirements

### Requirement: Factory Production Menu
MUST: システムは、工場、空港、港などの施設タイプによってフィルタリングされた動的な生産メニューを提供する。

#### Scenario: Factory properties show correct units
- **WHEN** ユーザーが工場のプロパティでアクションメニューを開いた場合
- **THEN** SHALL: システムは、陸上車両と歩兵ユニットのみを含む生産メニューを表示する。

#### Scenario: Airport properties show correct units
- **WHEN** ユーザーが空港のプロパティでアクションメニューを開いた場合
- **THEN** SHALL: システムは、航空ユニットのみを含む生産メニューを表示する。

#### Scenario: Port properties show correct units
- **WHEN** ユーザーが港のプロパティでアクションメニューを開いた場合
- **THEN** SHALL: システムは、艦船ユニットのみを含む生産メニューを表示する。
