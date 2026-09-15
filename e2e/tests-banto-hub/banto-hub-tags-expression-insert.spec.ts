/**
 * #342 段階C（docs/tag-server-design.md §4.2「一覧から挿入」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 追補）: 演算タグの式欄の
 * 「一覧から挿入」トグルと、新規作成フォームの右ペイン化の実 DOM
 * 受け入れテスト。
 *
 * このファイルを新設した理由: 段階Aの
 * `banto-hub-tags-expression-check.spec.ts` は式欄の**チェック結果表示**
 * だけを見ており、段階Cの本題（トグルが ON の間だけ行クリックの意味が
 * 変わる・キャレット位置へ挿入する・編集対象は切り替わらない・除外対象は
 * 理由のトーストを出す・Esc で解除）は誰も固定していない。
 *
 * issue #342 C の原案（`ConnectionTree` を3階層にしてツリーのタグを
 * クリックする）は採らず、#375 で非モーダルになったグリッドの行クリックで
 * 挿入する（2026-09-15 オーナー決定）。したがってこの spec は
 * 「ツリーで参照したいグループを選ぶ → グリッドに出た行をクリック」という
 * 流れをそのままなぞる - フォームが非モーダルでなければ成立しない操作
 * なので、これ自体が create の右ペイン化の回帰テストにもなっている。
 *
 * ファイル名は `banto-hub-smoke.spec.ts` より辞書順で後
 * （`banto-hub-auth.ts` の注記参照）で、段階Aの `...-expression-check` の
 * 直後に並ぶ。前提データは UI ではなく `page.request` で直接 REST を叩いて
 * 作る（`banto-hub-tags-expression-check.spec.ts` と同じパターン）。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-expr-insert-plc';
const GROUP_NAME = 'e2e-expr-insert-group';
const REF_TAG_NAME = 'e2e-expr-insert-ref';
const CALC_GROUP_NAME = 'e2e-expr-insert-calc-group';
const COMPUTED_TAG_NAME = 'e2e-expr-insert-computed';

const REF_EXTERNAL_NAME = `${CONNECTION_NAME}.${GROUP_NAME}.${REF_TAG_NAME}`;

/**
 * 挿入前の式。キャレットを `(` の直後（オフセット1）へ置いて挿入するので、
 * 「末尾へ追記」ではなく「キャレット位置へ挿入」であることを値で区別できる。
 * 挿入後は `(<完全名> ) * 2` という**有効な式**になるため、段階Aのチェックが
 * そのまま成功し参照タグ一覧に出る（テスト2）。
 */
const BASE_EXPRESSION = '( ) * 2';
const CARET_OFFSET = 1;
const EXPECTED_EXPRESSION = `(${REF_EXTERNAL_NAME} ) * 2`;

function waitForExpressionCheck(page: Page) {
	return page.waitForResponse(
		(res) =>
			res.url().includes('/api/tags/expression/check') &&
			res.request().method() === 'POST' &&
			res.status() === 200
	);
}

test.describe.serial('banto-hub 演算タグの式欄「一覧から挿入」 (#342 段階C)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ1: シミュレーションモードの PLC接続 + 収集グループ +
		// 式から参照する PLC タグ1件（実 PLC/実ネットワークへは繋がない）。
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
		// グループと、その配下の演算タグ1件（テスト4の「自タグ」役）。
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
		const calcGroup = (await calcGroupRes.json()) as { id: number };

		const computedTagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: COMPUTED_TAG_NAME,
				collectionGroupId: calcGroup.id,
				address: '',
				dataType: 'f32',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'computed',
				expression: `${REF_EXTERNAL_NAME} * 2`,
				retain: false
			}
		});
		expect(computedTagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 新規作成ペインでトグルを ON にし、行クリックでキャレット位置へ完全名が入る（フォーカスは式欄・編集対象は変わらない）', async () => {
		await page.goto('/tags');
		// `calc` 配下のグループから開いた新規登録は `tagKind === 'computed'`
		// に確定する（`tagOnboarding.ts::resolveRegistrationTarget`）ので、
		// 式欄とトグルが最初から出る。
		await groupNodeByName(page, CALC_GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();

		// #342 段階C: 新規作成も広幅では非モーダルの右ペイン
		// （`role="complementary"`）。
		const pane = page.getByRole('complementary', { name: '新規作成' });
		await expect(pane).toBeVisible();

		const expressionField = pane.getByLabel('式');
		await expect(expressionField).toBeVisible();

		const toggle = pane.getByTestId('tag-expression-insert-toggle');
		await expect(toggle).toBeVisible();
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');

		// 挿入先の式とキャレット位置を作る（`(` の直後 = オフセット1）。
		// ここで走る段階Aのチェック（`( ) * 2` は構文エラー）を先に受け切って
		// おく - 後段の `waitForExpressionCheck` が、挿入で発火したチェックでは
		// なくこちらの応答を掴んでしまわないようにするため。
		const initialCheck = waitForExpressionCheck(page);
		await expressionField.fill(BASE_EXPRESSION);
		await initialCheck;
		await expressionField.evaluate(
			(el: HTMLTextAreaElement, offset: number) => el.setSelectionRange(offset, offset),
			CARET_OFFSET
		);

		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');
		// グリッド側の状態表示（クリックの意味が変わっていることが分かる）。
		await expect(page.getByTestId('tag-insert-armed-badge')).toBeVisible();

		// ツリーで参照したいタグのグループへ切り替える（ペインは非モーダル
		// なのでそのまま操作できる - #375）。ペインは閉じない。
		await groupNodeByName(page, GROUP_NAME).click();
		await expect(pane).toBeVisible();

		const checkResponse = waitForExpressionCheck(page);
		await page.getByRole('gridcell', { name: REF_TAG_NAME, exact: true }).click();

		// キャレット位置へ挿入されている（末尾追記ではない）。
		await expect(expressionField).toHaveValue(EXPECTED_EXPRESSION);
		// フォーカスは式欄に残り、キャレットは挿入した文字列の直後。
		await expect(expressionField).toBeFocused();
		const selectionStart = await expressionField.evaluate(
			(el: HTMLTextAreaElement) => el.selectionStart
		);
		expect(selectionStart).toBe(CARET_OFFSET + REF_EXTERNAL_NAME.length);

		// 編集対象は切り替わらない（ペインの見出しは「新規作成」のまま、
		// 「… を編集」のペインは現れない）。
		await expect(pane).toBeVisible();
		await expect(page.getByRole('complementary', { name: `${REF_TAG_NAME} を編集` })).toHaveCount(
			0
		);

		// 挿入が `input` イベントとして流れ、段階Aのチェックが発火している。
		await checkResponse;
	});

	test('2. 挿入後は段階Aのプレビューの参照タグ一覧にそのタグが出る', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const preview = pane.getByTestId('expression-preview');
		await expect(preview).toBeVisible();
		await expect(preview).toContainText('結果型');
		await expect(preview).toContainText(REF_EXTERNAL_NAME);
	});

	test('3. トグルを OFF に戻すと、行クリックは従来どおり編集対象の切り替えになる', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const toggle = pane.getByTestId('tag-expression-insert-toggle');
		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');
		await expect(page.getByTestId('tag-insert-armed-badge')).toHaveCount(0);

		// 新規作成フォームは入力済み（dirty）なので、行クリックでの編集対象
		// 切り替えには従来どおり破棄確認が挟まる（`selectTag`）。
		page.once('dialog', (dialog) => {
			void dialog.accept();
		});
		await page.getByRole('gridcell', { name: REF_TAG_NAME, exact: true }).click();

		await expect(page.getByRole('complementary', { name: `${REF_TAG_NAME} を編集` })).toBeVisible();
		await expect(pane).toHaveCount(0);
	});

	test('4. 編集ペインで自タグの行をクリックしても挿入されず、理由のトーストが出る', async () => {
		// 演算タグを編集対象にする（`calc` グループへ切り替えてから行クリック）。
		await groupNodeByName(page, CALC_GROUP_NAME).click();
		await page.getByRole('gridcell', { name: COMPUTED_TAG_NAME, exact: true }).click();

		const pane = page.getByRole('complementary', { name: `${COMPUTED_TAG_NAME} を編集` });
		await expect(pane).toBeVisible();

		const expressionField = pane.getByLabel('式');
		await expect(expressionField).toHaveValue(`${REF_EXTERNAL_NAME} * 2`);

		const toggle = pane.getByTestId('tag-expression-insert-toggle');
		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');

		await page.getByRole('gridcell', { name: COMPUTED_TAG_NAME, exact: true }).click();

		// 挿入されず、理由がトーストで出る。編集対象も切り替わらない。
		await expect(page.getByText('自分自身は式から参照できません')).toBeVisible();
		await expect(expressionField).toHaveValue(`${REF_EXTERNAL_NAME} * 2`);
		await expect(pane).toBeVisible();
	});

	test('5. Esc でトグルは OFF になり、ペインは閉じない', async () => {
		const pane = page.getByRole('complementary', { name: `${COMPUTED_TAG_NAME} を編集` });
		const toggle = pane.getByTestId('tag-expression-insert-toggle');
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');

		await page.keyboard.press('Escape');

		await expect(toggle).toHaveAttribute('aria-pressed', 'false');
		await expect(page.getByTestId('tag-insert-armed-badge')).toHaveCount(0);
		// #375 で決めたとおり、Esc はペイン自体を閉じない。
		await expect(pane).toBeVisible();
	});
});
