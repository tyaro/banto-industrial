/**
 * relay-wright 取扱説明書（manual.md）用スクリーンショット一括生成スクリプト。
 *
 * e2e/playwright.config.ts と同じ「browser vehicle」パターンで
 * `relay-wright-serve --features embed-ui` を一時DBに対して起動し、
 * デモデータを投入してから各画面を Playwright で撮影する。
 *
 * 再実行手順（リポジトリルートで）:
 *
 *   pnpm --filter relay-wright build
 *   cargo build -p relay-wright-core --bin relay-wright-serve --features embed-ui
 *   node apps/relay-wright/docs/capture-screenshots.mjs
 *
 * 出力: apps/relay-wright/docs/images/*.png（1400x900・ライトテーマ）
 *
 * 動作の流れ:
 *  1. 一時ディレクトリのSQLiteに対してサーバーを起動（BANTO_ALLOW_SETUP=1）
 *  2. 初回セットアップ画面を撮影 → REST で管理者アカウントを作成 → 停止
 *  3. node:sqlite で書き込み監査ログのデモ行だけを直接投入
 *     （監査ログはエンジンだけが書く append-only ログで登録 REST が無いため。
 *     PLC接続・収集グループ・タグは R1-B で REST が入ったので手順4で実経路）
 *  4. サーバー再起動 → REST で PLC接続・収集グループ・タグ・書き込み先・
 *     書き込みルールを作成（実経路を通す）
 *  5. Playwright でログインし、各画面を撮影
 *  6. サーバー停止・一時DB削除
 */
import { chromium } from '@playwright/test';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';

const dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '..', '..', '..');
const imagesDir = path.join(dirname, 'images');

const PORT = 8726;
const BASE_URL = `http://127.0.0.1:${PORT}`;

const ADMIN_USERNAME = 'admin';
const ADMIN_PASSWORD = 'RelayWright1';
const ADMIN_DISPLAY_NAME = '管理者';

const serveBin = path.join(
	repoRoot,
	'target',
	'debug',
	process.platform === 'win32' ? 'relay-wright-serve.exe' : 'relay-wright-serve'
);

const dbDir = fs.mkdtempSync(path.join(os.tmpdir(), 'relay-wright-manual-'));
const dbPath = path.join(dbDir, 'relay-wright-manual.sqlite3');

fs.mkdirSync(imagesDir, { recursive: true });

/** Spawn relay-wright-serve and wait until /api/auth/status responds. */
async function startServer() {
	const child = spawn(serveBin, [], {
		env: {
			...process.env,
			PORT: String(PORT),
			BANTO_BIND: '127.0.0.1',
			BANTO_DB: dbPath,
			BANTO_ALLOW_SETUP: '1'
		},
		stdio: 'ignore'
	});
	const deadline = Date.now() + 30_000;
	for (;;) {
		try {
			const res = await fetch(`${BASE_URL}/api/auth/status`, {
				headers: { 'X-Banto-Client': 'banto' }
			});
			if (res.ok) return child;
		} catch {
			/* not up yet */
		}
		if (Date.now() > deadline) throw new Error('server did not start within 30s');
		await new Promise((r) => setTimeout(r, 250));
	}
}

async function stopServer(child) {
	if (!child || child.exitCode !== null) return;
	const exited = new Promise((r) => child.once('exit', r));
	child.kill();
	await Promise.race([exited, new Promise((r) => setTimeout(r, 5000))]);
	if (child.exitCode === null) child.kill('SIGKILL');
	// Give SQLite a beat to release the file handle on Windows.
	await new Promise((r) => setTimeout(r, 500));
}

/** Minimal REST helper (X-Banto-Client CSRF header + optional bearer token). */
async function api(method, route, { token, body } = {}) {
	const headers = { 'X-Banto-Client': 'banto' };
	if (token) headers['Authorization'] = `Bearer ${token}`;
	if (body !== undefined) headers['Content-Type'] = 'application/json';
	const res = await fetch(`${BASE_URL}${route}`, {
		method,
		headers,
		body: body === undefined ? undefined : JSON.stringify(body)
	});
	const text = await res.text();
	if (!res.ok) throw new Error(`${method} ${route} -> ${res.status}: ${text}`);
	return text ? JSON.parse(text) : null;
}

/**
 * Seed write_audit_log demo rows directly into the SQLite file while the
 * server is stopped. The audit log is an append-only trail written only by
 * the engine (deliberately no create REST route), so direct INSERT is the
 * only way to stage demo rows. Everything else (PLC接続・収集グループ・タグ・
 * 書き込み先・ルール) is created over REST in phase B — the real paths.
 */
function seedDatabase() {
	const db = new DatabaseSync(dbPath);
	try {
		db.exec('BEGIN');
		// Write audit demo rows. Shapes mirror what the engine itself records
		// (core/src/engine/write_audit.rs / writer.rs): rule_fire /
		// rate_limit_tripped rows have no human actor; the rate-limit trip row
		// uses result 'suppressed_rate_limited' with the writer's detail text.
		// Rule/target ids 1..2 and source tag ids 1..3 match the rows created
		// over REST in phase B (fresh DB -> AUTOINCREMENT starts at 1).
		const audit = db.prepare(
			`INSERT INTO write_audit_log
			   (ts, write_rule_id, rule_name_snapshot, source_tag_id, source_value_snapshot,
			    write_target_id, target_value_written, actor_username, action, result, detail)
			 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`
		);
		const rows = [
			[
				'2026-07-30 08:55:12',
				null,
				'-',
				null,
				null,
				null,
				null,
				'admin',
				'dry_run_toggle',
				'ok',
				'{"dryRun":true}'
			],
			['2026-07-30 09:00:00', null, '-', null, null, null, null, 'admin', 'arm', 'ok', null],
			[
				'2026-07-30 09:02:31',
				1,
				'高温時冷却バルブON',
				1,
				81.5,
				1,
				1,
				null,
				'rule_fire',
				'suppressed_dry_run',
				null
			],
			[
				'2026-07-30 09:05:10',
				null,
				'-',
				null,
				null,
				null,
				null,
				'admin',
				'dry_run_toggle',
				'ok',
				'{"dryRun":false}'
			],
			[
				'2026-07-30 09:12:44',
				1,
				'高温時冷却バルブON',
				1,
				82.5,
				1,
				1,
				null,
				'rule_fire',
				'ok',
				null
			],
			['2026-07-30 09:13:02', 2, '停止中の警報リセット', 2, 0, 2, 1, null, 'rule_fire', 'ok', null],
			[
				'2026-07-30 09:20:18',
				1,
				'高温時冷却バルブON',
				1,
				85.1,
				1,
				1,
				null,
				'rule_fire',
				'failed',
				'PLC connection lost mid-write'
			],
			[
				'2026-07-30 09:25:40',
				1,
				'高温時冷却バルブON',
				1,
				86.0,
				1,
				1,
				null,
				'rate_limit_tripped',
				'suppressed_rate_limited',
				'rate limit exceeded; breaker tripped and engine auto-disarmed'
			],
			[
				'2026-07-30 09:25:41',
				1,
				'高温時冷却バルブON',
				1,
				86.2,
				1,
				1,
				null,
				'rule_fire',
				'suppressed_disarmed',
				null
			],
			['2026-07-30 09:30:00', null, '-', null, null, null, null, 'admin', 'disarm', 'ok', null]
		];
		for (const r of rows) audit.run(...r);
		db.exec('COMMIT');
	} finally {
		db.close();
	}
}

async function main() {
	let server = null;
	const browser = await chromium.launch();
	const page = await browser.newPage({
		viewport: { width: 1400, height: 900 },
		colorScheme: 'light',
		locale: 'ja-JP'
	});
	// window.confirm 等のネイティブダイアログは Playwright では撮影不可。
	// 誤クリック時に固まらないよう常に dismiss する。
	page.on('dialog', (dialog) => void dialog.dismiss());

	const shot = (name, options = {}) =>
		page.screenshot({ path: path.join(imagesDir, name), ...options });

	try {
		// ---- Phase A: fresh DB -> setup screen shot -> create admin via REST.
		server = await startServer();
		await page.goto(`${BASE_URL}/login`);
		await page.getByLabel('表示名').waitFor();
		await shot('setup.png');

		const setup = await api('POST', '/api/auth/setup', {
			body: {
				username: ADMIN_USERNAME,
				password: ADMIN_PASSWORD,
				displayName: ADMIN_DISPLAY_NAME
			}
		});
		if (!setup.success) throw new Error(`setup failed: ${JSON.stringify(setup)}`);
		// 2人目のユーザー（ユーザー管理画面に複数行を出すため）。
		await api('POST', '/api/users', {
			token: setup.token,
			body: {
				username: 'operator',
				password: 'OperatorPass1',
				displayName: '現場オペレーター',
				role: 'editor'
			}
		});

		// ---- Seed write audit demo rows (server stopped).
		await stopServer(server);
		server = null;
		seedDatabase();

		// ---- Phase B: restart, create the registry (PLC接続 → 収集グループ →
		// タグ → 書き込み先 → ルール) over the real REST paths.
		server = await startServer();
		const login = await api('POST', '/api/auth/login', {
			body: { username: ADMIN_USERNAME, password: ADMIN_PASSWORD }
		});
		if (!login.success) throw new Error(`login failed: ${JSON.stringify(login)}`);
		const token = login.token;

		const connection = await api('POST', '/api/plc-connections', {
			token,
			body: {
				name: 'ライン1 PLC',
				protocol: 'slmp',
				host: '192.0.2.10',
				port: 5007,
				unitId: 1,
				enabled: true
			}
		});
		const group = await api('POST', '/api/collection-groups', {
			token,
			body: {
				name: 'ライン1 収集グループ',
				plcConnectionId: connection.id,
				periodMs: 1000,
				enabled: true
			}
		});
		const tagBody = (name, address, dataType, unit, decimals) => ({
			name,
			collectionGroupId: group.id,
			address,
			dataType,
			unit,
			decimals,
			enabled: true
		});
		const tag1 = await api('POST', '/api/tags', {
			token,
			body: tagBody('温度センサ', 'D100', 'i16', '℃', 1)
		});
		const tag2 = await api('POST', '/api/tags', {
			token,
			body: tagBody('運転状態', 'M10', 'bit', null, 0)
		});
		const tag3 = await api('POST', '/api/tags', {
			token,
			body: tagBody('圧力センサ', 'D110', 'i16', 'kPa', 0)
		});

		const target1 = await api('POST', '/api/write-targets', {
			token,
			body: {
				name: '冷却バルブ指令',
				plcConnectionId: connection.id,
				address: 'D200',
				dataType: 'i16',
				unit: null,
				decimals: 0,
				rawLo: null,
				rawHi: null,
				engLo: null,
				engHi: null,
				enabled: true
			}
		});
		const target2 = await api('POST', '/api/write-targets', {
			token,
			body: {
				name: '警報リセット',
				plcConnectionId: connection.id,
				address: 'M50',
				dataType: 'bit',
				unit: null,
				decimals: 0,
				rawLo: null,
				rawHi: null,
				engLo: null,
				engHi: null,
				enabled: true
			}
		});
		await api('POST', '/api/write-rules', {
			token,
			body: {
				name: '高温時冷却バルブON',
				enabled: true,
				edgeMode: 'rising',
				cooldownMs: null,
				writeTargetId: target1.id,
				writeValueMode: 'constant',
				writeConstantValue: 1,
				writeSourceTagId: null,
				conditions: [
					{ sourceTagId: tag1.id, operator: 'gt', thresholdValue: 80, thresholdValue2: null }
				]
			}
		});
		await api('POST', '/api/write-rules', {
			token,
			body: {
				name: '停止中の警報リセット',
				enabled: true,
				edgeMode: 'falling',
				cooldownMs: 60000,
				writeTargetId: target2.id,
				writeValueMode: 'constant',
				writeConstantValue: 1,
				writeSourceTagId: null,
				conditions: [
					{ sourceTagId: tag2.id, operator: 'eq', thresholdValue: 0, thresholdValue2: null },
					{ sourceTagId: tag3.id, operator: 'lt', thresholdValue: 200, thresholdValue2: null }
				]
			}
		});

		// QRコード画面のデモ文字列（タッチパネル読み取りデバッグ支援）。
		for (const [label, text] of [
			['開始', 'START'],
			['停止', 'STOP'],
			['リセット', 'RESET'],
			['デバッグモード', 'MODE:DEBUG']
		]) {
			await api('POST', '/api/qr-strings', { token, body: { label, text } });
		}

		// ---- Screens.
		// login.png: セットアップ済みDBなので通常のログインフォームが出る。
		await page.goto(`${BASE_URL}/login`);
		await page.getByRole('button', { name: 'ログイン' }).waitFor();
		await shot('login.png');

		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード', { exact: true }).fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		await page.waitForURL('**/settings');

		// エンジン制御・監視（非アーム状態のバッジ＋操作ボタン）。
		await page.goto(`${BASE_URL}/engine`);
		await page.getByText('DISARMED（非アーム）').waitFor();
		await shot('engine.png');
		// アーム確認ダイアログ（window.confirm）はネイティブダイアログのため
		// Playwright では撮影不可 -> engine-arm-confirm.png はスキップし、
		// manual.md に文言を記載する。

		// PLC接続（R1-B。新規作成フォーム + 一覧に1行）。
		await page.goto(`${BASE_URL}/plc-connections`);
		await page.getByText('ライン1 PLC').waitFor();
		await shot('plc-connections.png', { fullPage: true });

		// タグ登録（リストメイン。ツールバー + 一覧3行。グループ名は一覧の
		// 収集グループ列に出る）。
		await page.goto(`${BASE_URL}/tags`);
		await page.getByText('温度センサ').first().waitFor();
		await page.getByText('ライン1 収集グループ').first().waitFor();
		await shot('tags.png', { fullPage: true });

		// 一括登録（貼り付け）モーダル: グループを選び、Excel風（タブ区切り）
		// 2行 + CSV風（カンマ区切り）1行のデモを貼り付けてプレビューを出した
		// 状態。3行目はデータ型が無効（word）で、行別検証エラーの表示例を兼ねる。
		// オーバーレイは fixed なので fullPage ではなくビューポートで撮る。
		await page.getByRole('button', { name: '一括登録（貼り付け）' }).click();
		// モーダル内は select / textarea が各1つなので要素ロケーターで十分
		// （ラベルは <label> 内包型で、アクセシブルネーム計算に頼らない）。
		const bulkDialog = page.getByRole('dialog', { name: '一括登録（貼り付け）' });
		await bulkDialog
			.locator('select')
			.selectOption({ label: 'ライン1 収集グループ（ライン1 PLC）' });
		await bulkDialog
			.locator('textarea')
			.fill(
				'流量センサ\tD120\ti32\tL/min\t1\n稼働カウンタ\tD130\tu32\t回\t\n異常コード,D140,word,,0'
			);
		await bulkDialog.getByText('3件中2件登録可能').waitFor();
		await shot('tags-bulk-paste.png');
		// 撮影のみ（登録はしない）— Esc でモーダルを閉じて次の画面へ。
		await page.keyboard.press('Escape');

		// 連続登録モーダル: グループを選び、開始 D200・件数8・i16（既定）で
		// 連番プレビュー（D200〜D207・名前=アドレス自動割り付け）を出した状態。
		await page.getByRole('button', { name: '連続登録' }).click();
		const seqDialog = page.getByRole('dialog', { name: '連続登録' });
		await seqDialog
			.locator('select')
			.first()
			.selectOption({ label: 'ライン1 収集グループ（ライン1 PLC）' });
		// 開始デバイスは placeholder で特定（type="text" は単位欄と2つある）。
		await seqDialog.getByPlaceholder('D100').fill('D200');
		await seqDialog.locator('input[type="number"]').first().fill('8');
		await seqDialog.getByText('D200 〜 D207（i16, step1, 8件）を登録します').waitFor();
		await shot('tags-sequential.png');
		// 撮影のみ（登録はしない）。
		await page.keyboard.press('Escape');

		// 書き込み先（グリッドに2行）。
		await page.goto(`${BASE_URL}/write-targets`);
		await page.getByText('冷却バルブ指令').waitFor();
		await shot('write-targets.png', { fullPage: true });
		// 行クリックで編集パネルを開いた状態。
		await page.getByText('冷却バルブ指令').click();
		await page.getByRole('heading', { name: '冷却バルブ指令 を編集' }).waitFor();
		await page.getByRole('heading', { name: '冷却バルブ指令 を編集' }).scrollIntoViewIfNeeded();
		await shot('write-targets-form.png', { fullPage: true });

		// 書き込みルール（グリッド + インライン条件エディタ）。
		await page.goto(`${BASE_URL}/write-rules`);
		await page.getByText('高温時冷却バルブON').first().waitFor();
		await shot('write-rules.png', { fullPage: true });
		await page.getByText('停止中の警報リセット').first().click();
		await page.getByRole('heading', { name: '停止中の警報リセット を編集' }).waitFor();
		await shot('write-rules-form.png', { fullPage: true });

		// 書き込み監査ログ（結果の色分け）。1行クリックで詳細パネルも出す。
		await page.goto(`${BASE_URL}/write-audit-log`);
		await page.getByText('レート制限トリップ').first().waitFor();
		await shot('write-audit-log.png');

		// QRコード（デバッグ支援）。管理リストとQRタイルグリッドの両方が
		// 入るよう fullPage で撮る（SVGはサーバー生成なので待つのは描画のみ）。
		await page.goto(`${BASE_URL}/qr-codes`);
		await page.getByText('デバッグモード').first().waitFor();
		await page.locator('.qr-svg svg').first().waitFor();
		await shot('qr-codes.png', { fullPage: true });

		// 操作監査ログ（M14）。Header の h1 も同じ「監査ログ」を描画するので
		// level: 2 でページ本体の h2 に絞る（chronogazer smoke.spec.ts と同じ理由）。
		await page.goto(`${BASE_URL}/audit-log`);
		await page.getByRole('heading', { level: 2, name: '監査ログ' }).waitFor();
		await page.getByText('件の記録があります').waitFor();
		// グリッド行の描画をひと呼吸待つ。
		await page.waitForTimeout(500);
		await shot('audit-log.png');

		// ユーザー管理（2ユーザー）。
		await page.goto(`${BASE_URL}/users`);
		await page.getByText('現場オペレーター').waitFor();
		await shot('users.png', { fullPage: true });

		// 設定。
		await page.goto(`${BASE_URL}/settings`);
		await page.getByRole('heading', { name: 'テーマ' }).waitFor();
		await shot('settings.png', { fullPage: true });

		console.log(`done. screenshots written to ${imagesDir}`);
	} finally {
		await browser.close();
		await stopServer(server);
		try {
			fs.rmSync(dbDir, { recursive: true, force: true });
		} catch (err) {
			// Windows では SQLite のファイルハンドル解放が僅かに遅れることがある。
			console.warn(`could not remove temp db dir ${dbDir}: ${err}`);
		}
	}
}

await main();
