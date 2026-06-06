# GLOSSARY.md - 用語集

本ドキュメントは、OpenWarsプロジェクト内で使用される主要な技術用語およびドメイン用語の定義をまとめたものです。

## A

### AP (Action Points)
ユニットが1ターンの間に行動（移動、攻撃など）を実行するためのコストリソース。行動ごとに消費され、次のターン開始時に回復します。

### anyhow
Rustのエラーハンドリング用クレート。アプリケーション層やプレゼンテーション層でエラーを集約し、コンテキスト情報を付与するために使用します。

## B

### Bevy / bevy_ecs
Rustで記述されたデータ駆動型のゲームエンジン。本プロジェクトでは、コアロジックの管理にBevyのECS機能（`bevy_ecs`クレート）のみを独立して利用しています。

## C

### Component (コンポーネント)
ECSアーキテクチャにおけるデータ単位。エンティティに付与される属性（例: `GridPosition`, `Health`, `ActionPoints`）であり、ロジックは持ちません。

### CUI (Character User Interface)
ターミナル（コマンドプロンプトやPowerShell等）上で動作するテキストベースのユーザーインターフェース。本プロジェクトの第一段階では `cli` クレートとしてCUI版を提供します。

### Command (コマンド)
ユーザー（またはAI）の入力に基づいて、システムに変更を要求するオブジェクト。UI層からエンジン層へ発行されます。

## D

### Domain-Driven Design (DDD)
ドメイン駆動設計。ソフトウェアの設計において、対象となるビジネス領域（ドメイン）の概念をモデル化し、実装に落とし込む手法。本プロジェクトではドメインロジックの分離に活用しています。

## E

### ECS (Entity Component System)
ゲーム開発において主流となっているアーキテクチャパターン。
- **Entity**: 一意のID（実体）
- **Component**: データ
- **System**: ロジック
の3要素で構成され、データと処理を完全に分離します。

### Entity (エンティティ)
ECSにおける基本単位。一意のIDを持ち、複数のComponentを束ねる器としての役割を果たします。

### Event (イベント)
システム内で発生した事象（例: `UnitMovedEvent`, `DamageTakenEvent`）。エンジン層からUI層へ状態変化を非同期に通知するために使用されます。

## G

### GridPosition
マップ上での六角形（ヘックス）または四角形グリッドにおける座標を示す値オブジェクト。

### GUI (Graphical User Interface)
グラフィカルなユーザーインターフェース。本プロジェクトの第二段階として、Tauriを用いたGUI版（`gui` クレート）の開発を予定しています。

## H

### Hex (ヘックス)
六角形のマス。戦略ゲームにおけるマップの基本構成単位。

### HitPoint (HP)
ユニットや拠点の耐久値。0になるとユニットは破壊・消滅します。

## N

### Newtype Pattern
Rustにおけるデザインパターンの一つ。既存の型（`uuid::Uuid` や `i32` など）をタプル構造体でラップし、コンパイラによる型チェックを強化する手法。

## R

### ratatui
Rust製のTUI（Terminal User Interface）構築ライブラリ。`cli` クレートでの画面描画に使用されます。

### Resource (リソース)
ECSアーキテクチャにおいて、エンティティに紐付かないグローバルなデータ（例: 現在のターン数、マップの地形データ、マスター設定）。

## S

### System (システム)
ECSアーキテクチャにおいて、特定のComponentを持つEntityに対して一括処理（ロジック）を実行する関数。

## T

### Tauri
RustとWeb技術（HTML/CSS/JS）を用いて、軽量で安全なデスクトップアプリケーションを構築するためのフレームワーク。

### thiserror
Rustのエラーハンドリング用クレート。ドメイン層やインフラ層で、型安全で明確なカスタムエラー（列挙型）を定義するために使用します。

## V

### Value Object (値オブジェクト)
DDDにおける概念。等価性が属性（値）によって決まり、一意の識別子を持たないオブジェクト。不変（Immutable）であることが推奨されます。

---

## 略語一覧

| 略語 | 正式名称 | 説明 |
| --- | --- | --- |
| AP | Action Points | 行動ポイント |
| CUI | Character User Interface | テキストベースのUI |
| ECS | Entity Component System | エンティティ・コンポーネント・システム |
| GUI | Graphical User Interface | グラフィカルUI |
| HP | Hit Points | 耐久値 |
| TUI | Terminal User Interface | ターミナルUI (CUIと同義) |
