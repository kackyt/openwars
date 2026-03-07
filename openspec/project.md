# Project Context

## Purpose

某ターン制戦略シミュレーションを模したゲームを作ることを目標に据える

## Tech Stack
フロントエンド
- CUI (Rust)
- GUI
  - Tauri (React)

ロジック
- Rust

## Project Conventions

### Code Style
- ソースコードにはロジックの内容がわかるように日本語のコメントをいれること

### Architecture Patterns
Reactはbullet proof reactでディレクトリは配置
RustはDDDを意識してモデル層を作る。オニオンアーキテクチャを意識すること

### Testing Strategy

ロジック単体テストは必ず実装する
GUIは後回し

### Git Workflow
GitHub Flowを採用する

## Domain Context

ゲーム概略
https://gbwn.main.jp/GBWT.htm

マップ一覧
https://gbwn.main.jp/Normal_GBWT.htm


## Important Constraints

## External Dependencies
