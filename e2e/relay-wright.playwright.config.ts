/**
 * Playwright config for apps/relay-wright's own E2E suite (H5 の残作業、
 * docs/deps-integration-plan.md — フロントテスト基盤(vitest) + hub/relay-wright
 * E2E のうち relay-wright 分)。
 *
 * 調査の結果、docs にあった「Tauri 依存で WebDriver 検討要」は実態より
 * 悲観的だった: `apps/relay-wright/src/lib/banto/setup.ts` の doc comment の
 * とおり、relay-wright のフロントは3つの環境を明示的にサポートする
 * （1. Tauri webview、2. 組み込みサーバーが配信する LAN ブラウザ
 * （`fetch()`/REST + SSE）、3. 素の `vite dev`）。UI コードは環境で分岐せず
 * プロバイダ層が吸収する設計なので、**モード2**（`relay-wright-serve` が
 * 配信する Tauri 不要の開発用ビークル）に対しては banto-hub と全く同じ形の
 * Playwright E2E が組める。**モード1（Tauri webview 固有の
 * `invoke()`分岐・`banto://event`・vibrancy 等）はこの config の対象外** -
 * それらは WebDriver が要る別課題として H5 のスコープから明示的に分離する
 * （オーナー承認済み方針）。
 *
 * `e2e/banto-hub.playwright.config.ts` の分離設計をそのまま踏襲している。
 * relay-wright は Tauri アプリだが、`relay-wright-serve`
 * （`apps/relay-wright/core/src/bin/relay-wright-serve.rs`）は Tauri を
 * 使わない headless axum サーバー単体のバイナリなので、banto-hub と同様に
 * `webServer` はこのバイナリをそのまま起動する（`cargo run` ではなく
 * 既にビルド済みのバイナリ — 起動をほぼ瞬時にし、テスト実行中の不意な
 * 再コンパイルを避ける、banto-hub/chronogazer 側と同じ理由）。
 * `pnpm --filter relay-wright build` と
 * `cargo build -p relay-wright-core --bin relay-wright-serve --features embed-ui`
 * はこのファイルの外（README/CI ワークフロー）で先に実行しておくこと。
 *
 * chronogazer の `playwright.config.ts`/`global-teardown.ts`/`tests/` と
 * banto-hub の `banto-hub.playwright.config.ts`/`global-teardown-banto-hub.ts`/
 * `tests-banto-hub/` とは、ポート・一時DB・テスト用ディレクトリ・出力先
 * ディレクトリを全て分離してある（下記）ので、3つの `pnpm e2e*` を同一
 * マシンで独立に実行できる。**chronogazer 用ファイルと banto-hub 用ファイルは
 * この config から一切参照・変更しない。**
 *
 * `testDir` を `e2e/tests/`・`e2e/tests-banto-hub/` ではなく専用の
 * `e2e/tests-relay-wright/` にしているのも同じ分離目的:
 * banto-hub 側の doc comment に書かれている実測済みの回帰
 * （chronogazer 側の `playwright.config.ts` は `testDir: './tests'` を
 * `testMatch` で絞り込んでいないため、他アプリ用の spec を
 * `e2e/tests/` に置くと `pnpm e2e` がそちらの `webServer` に対して誤って
 * 実行してしまい初回セットアップ前提が壊れる）が、`tests-banto-hub/` を
 * 増やしても同様に起こりうるので、relay-wright 用は3つ目の専用ディレクトリ
 * として分離する。
 */
import { defineConfig, devices } from '@playwright/test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '..');

// chronogazer は 8798（e2e/playwright.config.ts）、banto-hub は 8799
// （e2e/banto-hub.playwright.config.ts）: relay-wright はその隣の 8800 を
// 使い、同一マシンで3つの `webServer` を同時に起動しても衝突しない。
const PORT = 8800;
const BASE_URL = `http://127.0.0.1:${PORT}`;

// chronogazer の `BANTO_E2E_DB_DIR`/banto-hub の `BANTO_HUB_E2E_DB_DIR` と
// 同じ理由（SqliteConnectOptions::create_if_missing はファイルは作るが親
// ディレクトリは作らない）で、一時ディレクトリ自体を先に用意してから
// `BANTO_DB` に渡す。env 変数名は `RELAY_WRIGHT_E2E_DB_DIR` -
// chronogazer/banto-hub 側の変数名と衝突しない別名にして、
// `global-teardown-relay-wright.ts` が誤って他の2つの一時ディレクトリを
// 消してしまわないようにする。
const dbDir = fs.mkdtempSync(path.join(os.tmpdir(), 'relay-wright-e2e-'));
const dbPath = path.join(dbDir, 'relay-wright-e2e.sqlite3');
process.env.RELAY_WRIGHT_E2E_DB_DIR = dbDir;

const relayWrightServeBin = path.join(
	repoRoot,
	'target',
	'debug',
	process.platform === 'win32' ? 'relay-wright-serve.exe' : 'relay-wright-serve'
);

export default defineConfig({
	testDir: './tests-relay-wright',
	// `testDir` 自体が chronogazer/banto-hub と分離済み(上記)だが、命名規則も
	// `relay-wright-*.spec.ts` に絞っておく - 将来 `tests-relay-wright/` に
	// 非 spec のヘルパー以外のファイルが増えても誤って拾わない保険
	// （banto-hub 側と同じ理由）。
	testMatch: 'relay-wright-*.spec.ts',
	// 出力先も他の2つと別ディレクトリ（`-relay-wright` サフィックス付き）に
	// する - 同一マシンで3つの `pnpm e2e*` を実行しても互いの結果を上書き
	// しない。
	outputDir: path.join(dirname, 'test-results-relay-wright'),
	globalTeardown: path.join(dirname, 'global-teardown-relay-wright.ts'),
	fullyParallel: false,
	workers: 1,
	retries: process.env.CI ? 1 : 0,
	reporter: process.env.CI
		? [
				['github'],
				[
					'html',
					{ open: 'never', outputFolder: path.join(dirname, 'playwright-report-relay-wright') }
				]
			]
		: [['list']],
	expect: {
		timeout: 10_000
	},
	use: {
		baseURL: BASE_URL,
		trace: 'retain-on-failure',
		screenshot: 'only-on-failure'
	},
	projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
	webServer: {
		command: relayWrightServeBin,
		url: BASE_URL,
		// 前回実行の(既にセットアップ済みの)DB を引き継ぐと、setup 画面の
		// 「ユーザー0件」前提が崩れる - chronogazer/banto-hub 側と同じ理由で
		// 常に新規サーバー/新規DBを起動する。
		reuseExistingServer: false,
		timeout: 30_000,
		env: {
			PORT: String(PORT),
			// `relay-wright-serve.rs` の既定 bind は `0.0.0.0`（LAN からの
			// 到達性デモが目的のツールなので - 同ファイルの doc comment
			// 参照）。banto-hub/chronogazer と揃えて E2E では明示的に
			// 127.0.0.1 に絞る（テスト実行環境のファイアウォール確認
			// ダイアログや外部からの到達を避ける）。
			BANTO_BIND: '127.0.0.1',
			BANTO_DB: dbPath,
			// apps/relay-wright/core/src/bin/relay-wright-serve.rs: POST
			// /api/auth/setup は明示的に opt-in しないと 403 になる - 初回
			// セットアップ画面のシナリオに必要。
			BANTO_ALLOW_SETUP: '1'
		}
	}
});
