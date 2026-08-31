# e2e/ — Playwright E2E スイート

このリポジトリには独立した4つの Playwright config がある。いずれも「LAN/REST モードの実サーバーに対する DOM テスト」で、モックした frontend ではない。ポート・一時 DB・出力ディレクトリは互いに分離してあるので、同一マシンで独立に実行できる。

| config                                | 対象                                                     | コマンド                  | CI             |
| ------------------------------------- | -------------------------------------------------------- | ------------------------- | -------------- |
| `playwright.config.ts`                | ChronoGazer smoke                                        | `pnpm e2e`                | ✅             |
| `banto-hub.playwright.config.ts`      | banto-hub 本体 (T18 機能一式)                            | `pnpm e2e:banto-hub`      | ✅             |
| `banto-hub-perf.playwright.config.ts` | banto-hub 性能計測（後述）                               | `pnpm e2e:banto-hub:perf` | ❌ opt-in のみ |
| `relay-wright.playwright.config.ts`   | relay-wright（LAN/REST モード = spec §11.1 モード2のみ） | `pnpm e2e:relay-wright`   | ✅             |

relay-wright は Tauri アプリだが、`relay-wright-serve`（`apps/relay-wright/core/src/bin/relay-wright-serve.rs`）という Tauri 不要の組み込みサーバー用バイナリ（spec §11.1 のモード2 = LAN ブラウザ／`HttpDataProvider`+`SseEventProvider`）を持つため、banto-hub と同じ形の Playwright E2E をそのまま組める（詳細は `relay-wright.playwright.config.ts` の doc comment）。Tauri webview 固有の経路（モード1: `invoke()` 分岐・`banto://event`・vibrancy 等）はこの config の対象外で、WebDriver が要る別課題として切り分けてある。

## ポート割り当て

| アプリ         | ポート |
| -------------- | ------ |
| chronogazer    | 8798   |
| banto-hub      | 8799   |
| relay-wright   | 8800   |
| banto-hub-perf | 8801   |

## ビルド前提

いずれの config も `webServer` は `cargo run` ではなく**ビルド済みバイナリ**を直接起動する（起動をほぼ瞬時にし、テスト実行中の不意な再コンパイルを避けるため）。実行前に:

```sh
pnpm install
pnpm build                                                          # フロントの静的ビルド（assets.rs が embed する）
cargo build -p chronogazer-core --bin banto-serve --features embed-ui   # pnpm e2e 用
cargo build -p banto-hub-core --bin banto-hub --features embed-ui       # pnpm e2e:banto-hub / :perf 用
cargo build -p relay-wright-core --bin relay-wright-serve --features embed-ui  # pnpm e2e:relay-wright 用
pnpm exec playwright install chromium                               # 初回のみ
```

## banto-hub 性能計測ハーネス（T18-5a 第2段、opt-in）

`docs/banto-hub-t18-design.md` §4 決定3/決定6 の性能目標を実測するハーネス。**CI では走らない**（`banto-hub.playwright.config.ts` の `testMatch: 'banto-hub-*.spec.ts'` に一致しないファイル名 `e2e/tests-banto-hub-perf/perf-tags-10k.spec.ts` を使い、専用 config からしか拾われない）。10,000 タグ・500 グループを seed するため使い捨ての専用 DB（一時ディレクトリ、実行後に自動削除）・専用ポート（8801）で動く — 本体 e2e や chronogazer の DB とは一切共有しない。

実行:

```sh
pnpm e2e:banto-hub:perf
```

計測する3項目（いずれも `console.log`/`console.warn` に出るほか、`e2e/perf-results/perf-tags-10k-<timestamp>.json` にも書き出す。`.gitignore` 済みなのでコミットされない）:

1. **初期表示**: `/tags` へ遷移してからグリッド最初の行描画+検索ボックス操作可能になるまでを3回計測し、各回と中央値を表示（目標 2 秒）。
2. **検索 p50/p95**: 検索ボックスへ部分一致クエリ（毎回別語・ヒット20件で固定）を入力してから件数表示（`N / 10000 件`）が更新されるまでを20回サンプリングし、p50/p95 を表示（目標 p95 100ms）。
3. **連続登録1,000件**: UI の連続登録ドロワーから 1,000 件（`MAX_CONTINUOUS_COUNT`）の dry-run 検証・適用それぞれの所要時間を計測（目標 各5秒）。

**性能目標は `expect` でハードアサートしない** — 実行環境（CPU/メモリ/ディスク）に数値が強く依存するため、未達は WARN 表示に留め、計測が完了すれば spec 自体は pass する。結果の解釈は決定3の基準機（Intel Core i5 第11世代・メモリ8〜16GB）を基準に、それ以外の環境での実行は参考値として扱うこと（spec の出力にも実行環境の CPU/メモリを添えている）。

seed（PLC接続20件・収集グループ500件・タグ10,000件を `POST /api/tags/batch` へ1,000件ずつ分割投入）自体の所要時間もログに出るが、計測対象（性能目標の対象）ではない。
