# banto-industrial

工場・現場系アプリのための再利用資産層。
[banto](https://github.com/tyaro/banto)（汎用管理画面テンプレート）の上に、
ドメイン寄りの共通資産（タグレジストリ・PLC通信・時系列収集/読み出し）と
それを使う製品アプリ（記録計ほか）を蓄積する。

- 計画: [docs/plan.md](docs/plan.md)（I系 = 資産クレート、R系 = 記録計）
- 記録計 要件定義: [docs/recorder-requirements.md](docs/recorder-requirements.md)
- banto 側のスコープ整理: banto リポジトリの docs/template-scope.md

## 構成（予定）

```
crates/
  banto-tags/        I1: タグレジストリ（定義・型・スケーリング）
  banto-slmp/        I2: MELSEC MC/SLMP クライアント（読み取り専用）
  banto-collect/     I3: 収集エンジン + 時系列ストレージ
  banto-tsquery/     I4: 期間クエリ + サーバ側間引き
apps/
  recorder/          R系: デジタル記録計（Tauri + LAN、banto テンプレート由来）
```

banto のパッケージ/クレートを消費する側: `@banto/*` は GitHub Packages、
クレートは git タグ参照（banto の docs/publishing.md 参照）。

## 権利

本リポジトリは自社著作物（All rights reserved）。案件アプリへは
依存ライブラリとして利用許諾で提供し、譲渡対象に含めない
（docs/plan.md §2）。
