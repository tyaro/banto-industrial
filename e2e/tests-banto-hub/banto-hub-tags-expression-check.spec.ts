/**
 * #342 段階A（docs/tag-server-design.md §4.2「演算タグの式チェック API」）:
 * 演算タグの式欄（`#tag-expression`）のライブチェック - エラー位置の下線
 * 表示・`POST /api/tags/expression/check` によるプレビュー（結果型・参照
 * タグ一覧・試算値）・エラーメッセージのクリックでキャレットが移動する
 * ことの実 DOM 受け入れテスト。
 *
 * ファイル名について（`banto-hub-auth.ts` の doc comment・
 * `e2e/tests/user-settings-routes.spec.ts` の前例と同じ理由）: このスイートは
 * `banto-hub.playwright.config.ts` の `testMatch: 'banto-hub-*.spec.ts'` を
 * `workers: 1`/`fullyParallel: false` でファイル名の辞書順に実行し、
 * `banto-hub-smoke.spec.ts` が最初の1件目として初回セットアップ（管理者
 * アカウント作成）を行う想定。このファイルは `ensureLoggedIn` 相当
 * （`fetchAuthToken`/`injectAuthToken`）でログインするだけなので、辞書順で
 * `banto-hub-smoke.spec.ts` より後になる名前が必要 - 既存の `banto-hub-tags-*`
 * 系スペック群と同じプレフィックスを踏襲しているため自然に満たされる
 * （`t` > `s`）。
 *
 * `banto-hub-tags-delete-impact.spec.ts` と同じパターン: 別 `describe.serial`
 * ブロック（別 `page`）、認証・前提データ（PLC接続・収集グループ・PLCタグ・
 * `calc` 予約接続配下の収集グループ）は `page.request` で直接 REST を叩いて
 * 作る。演算タグの保存そのものはこのスペックの対象外（チェック API と
 * フロントのライブ反映だけを見る）なので、作成した `calc` グループへは
 * 一度もタグを保存しない。
 *
 * **試算値についての判断（指示に無い分岐、報告事項）**: 有効な式の参照先に
 * 実 PLC タグを使うと、シミュレータ/収集を動かしていないこの spec では値が
 * 常に Bad（`preview.evaluated: false`）になる。逆に参照0件の定数式
 * （`1 + 1` 等）なら常に評価できるが、その場合は「参照タグ一覧」欄を同時に
 * 確認できない。実装指示は「結果型と参照タグ一覧、試算値が出る」を1つの
 * シナリオとして書いているが、この2つ（参照タグ一覧を出す・評価済みの試算
 * 値を出す）を実 DOM で同時に満たすには、事前に PLC タグへ Good 品質の値を
 * 用意する必要があり（収集開始 or 内部タグへの書き込み + API キーの
 * write スコープ発行）、この spec の前提データ量に対して重い。ここでは
 * 「参照タグ一覧が出ること」は実タグ参照で、「試算値が出ること（評価
 * できる/できないの両方を含む「試算値」欄自体の表示）」は数値が実際に
 * 出る経路として別途 0 参照の定数式で確認する、という2段構成にした。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-expr-check-plc';
const GROUP_NAME = 'e2e-expr-check-group';
const REF_TAG_NAME = 'e2e-expr-check-ref';
const CALC_GROUP_NAME = 'e2e-expr-check-calc-group';

const REF_EXTERNAL_NAME = `${CONNECTION_NAME}.${GROUP_NAME}.${REF_TAG_NAME}`;

function waitForExpressionCheck(page: Page) {
	return page.waitForResponse(
		(res) =>
			res.url().includes('/api/tags/expression/check') &&
			res.request().method() === 'POST' &&
			res.status() === 200
	);
}

test.describe.serial('banto-hub 演算タグの式チェック API (#342 段階A)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ1: シミュレーションモードの PLC接続 + 収集グループ +
		// 演算タグの式から参照する PLC タグ1件（実 PLC/実ネットワークへは
		// 繋がない - `banto-hub-tags-delete-impact.spec.ts` と同じ理由）。
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: true
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };

		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: {
				name: GROUP_NAME,
				plcConnectionId: connection.id,
				periodMs: 1000,
				enabled: true
			}
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };

		const refTagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: REF_TAG_NAME,
				collectionGroupId: group.id,
				address: '40001',
				dataType: 'i16',
				unit: '℃',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(refTagRes.ok()).toBe(true);

		// 前提データ2: `calc` 予約接続（起動時に自動作成済み）配下の収集
		// グループ - ここへ演算タグの作成フォームを開く
		// （`banto-hub-tags-delete-impact.spec.ts` と同じ手順）。
		const connectionsRes = await page.request.get('/api/plc-connections', {
			headers: authedHeaders
		});
		expect(connectionsRes.ok()).toBe(true);
		const connections = (await connectionsRes.json()) as { id: number; name: string }[];
		const calcConnection = connections.find((c) => c.name === 'calc');
		if (!calcConnection) {
			throw new Error("予約接続 'calc' が見つかりません（サーバー起動時に自動作成される想定）");
		}

		const calcGroupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: {
				name: CALC_GROUP_NAME,
				plcConnectionId: calcConnection.id,
				periodMs: 1000,
				enabled: true
			}
		});
		expect(calcGroupRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 構文エラーのある式を入力すると下線とメッセージが出る', async () => {
		await page.goto('/tags');
		await groupNodeByName(page, CALC_GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		// #342 段階C: 新規作成フォームも広幅では非モーダルの右ペイン
		// （`<aside aria-label>` = role `complementary`）へ移った（#375 の編集
		// ペインと同じ型）ため `role="dialog"` では取れない。アクセシブル名
		// （`新規作成`）は Modal 時代と同じ。
		const drawer = page.getByRole('complementary', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		const expressionField = drawer.getByLabel('式');
		await expect(expressionField).toBeVisible();

		const checkResponse = waitForExpressionCheck(page);
		// `crates/banto-expr/tests/compile.rs::syntax_error_position_points_at_offending_token`
		// と同じ式 - `pos: 4` を指す構文エラーになる。
		await expressionField.fill('1 + ');
		await checkResponse;

		// エラー位置の下線（`.expr-error-char`、`+page.svelte` の
		// `.expr-field-wrap` doc comment参照）。
		await expect(drawer.locator('.expr-error-char')).toBeVisible();
		// エラーメッセージ本体（クリック可能なボタン、`#tag-expression-err`）。
		const errorButton = drawer.locator('#tag-expression-err');
		await expect(errorButton).toBeVisible();
		await expect(errorButton).toBeEnabled();
		await expect(errorButton).toContainText('構文エラー');
	});

	test('2. エラーメッセージをクリックするとキャレットが該当位置(4)へ移動する', async () => {
		// テスト1の続き - 同じ Drawer・同じ構文エラー状態を前提にする
		// （テスト間で `page` を共有する `describe.serial` の作法どおり）。
		const drawer = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = drawer.getByLabel('式');
		await expect(expressionField).toHaveValue('1 + ');

		const errorButton = drawer.locator('#tag-expression-err');
		await expect(errorButton).toBeEnabled();
		await errorButton.click();

		await expect(expressionField).toBeFocused();
		const selectionStart = await expressionField.evaluate(
			(el: HTMLTextAreaElement) => el.selectionStart
		);
		expect(selectionStart).toBe(4);
	});

	test('3. 有効な式に直すと結果型・参照タグ一覧が出て、エラー表示は消える', async () => {
		const drawer = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = drawer.getByLabel('式');

		const checkResponse = waitForExpressionCheck(page);
		await expressionField.fill(`${REF_EXTERNAL_NAME} + 1`);
		await checkResponse;

		// エラー表示は消える。
		await expect(drawer.locator('#tag-expression-err')).toHaveCount(0);
		await expect(drawer.locator('.expr-error-char')).toHaveCount(0);

		// プレビュー: 結果型 + 参照タグ一覧（型・単位）。
		const preview = drawer.locator('[data-testid="expression-preview"]');
		await expect(preview).toBeVisible();
		await expect(preview).toContainText('結果型');
		await expect(preview).toContainText('num');
		await expect(preview).toContainText(REF_EXTERNAL_NAME);
		await expect(preview).toContainText('i16');
		await expect(preview).toContainText('℃');
		// 試算値欄自体は出るが、シミュレータ/収集を動かしていないため値は
		// 評価できない（Bad）- このファイル冒頭の doc comment「試算値に
		// ついての判断」参照。
		await expect(preview).toContainText('試算値');
	});

	test('4. 参照0件の定数式は現在値なしで評価され、試算値に数値が出る', async () => {
		const drawer = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = drawer.getByLabel('式');

		const checkResponse = waitForExpressionCheck(page);
		await expressionField.fill('1 + 1');
		await checkResponse;

		const preview = drawer.locator('[data-testid="expression-preview"]');
		await expect(preview).toBeVisible();
		await expect(preview).toContainText('試算値');
		await expect(preview).toContainText('2');
	});
});
