# banto-industrial

工場・現場系アプリのための再利用資産層。
[banto](https://github.com/tyaro/banto)（汎用管理画面テンプレート）の上に、
ドメイン寄りの共通資産（タグレジストリ・PLC通信・時系列収集/読み出し）と
それを使う製品アプリ（記録計ほか）を蓄積する。

- 計画: [docs/plan.md](docs/plan.md)（I系 = 資産クレート、R系 = 記録計 **ChronoGazer**）
- ChronoGazer（記録計）要件定義: [docs/recorder-requirements.md](docs/recorder-requirements.md)
- banto 側のスコープ整理: banto リポジトリの docs/template-scope.md

## 構成（予定）

```
crates/
  banto-tags/        I1: タグレジストリ（定義・型・スケーリング）
  banto-plc/         I2: PLC通信（読み取り専用。trait + Modbus TCP 先行、MC/SLMP 続行）
  banto-collect/     I3: 収集エンジン + 時系列ストレージ
  banto-tsquery/     I4: 期間クエリ + サーバ側間引き
apps/
  chronogazer/       R系: デジタル記録計 ChronoGazer（Tauri + LAN、banto テンプレート由来）
```

banto のパッケージ/クレートの消費は **両方とも git タグ参照**
（2026-07-12 決定。GitHub 組織名 banto が取得不能だったため
レジストリ発行は棚上げ。banto の docs/publishing.md 参照）:

```sh
pnpm add "github:tyaro/banto#v0.1.0&path:packages/admin-core"
```

```toml
banto-core = { git = "https://github.com/tyaro/banto.git", tag = "v0.1.0" }
```

## 権利

本リポジトリは自社著作物（All rights reserved）。案件アプリへは
依存ライブラリとして利用許諾で提供し、譲渡対象に含めない
（docs/plan.md §2）。
