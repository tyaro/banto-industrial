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
 * #379 CI 対応（2026-09-16）: このスペックが行を名前でクリックする箇所は
 * すべて**直前にツリーで対象グループを選んで絞り込んでいる**ので、
 * `BantoGrid` の行仮想化（`banto-hub-tags-revision.spec.ts` 冒頭の doc
 * comment 参照 - 件数が増えると一覧の末尾行は DOM に無い）の影響を受けない。
 * 絞り込みを外す変更を入れないこと。
 *
 * ファイル名は `banto-hub-smoke.spec.ts` より辞書順で後
 * （`banto-hub-auth.ts` の注記参照）で、段階Aの `...-expression-check` の
 * 直後に並ぶ。前提データは UI ではなく `page.request` で直接 REST を叩いて
 * 作る（`banto-hub-tags-expression-check.spec.ts` と同じパターン）。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-expr-insert-plc';
const GROUP_NAME = 'e2e-expr-insert-group';
const REF_TAG_NAME = 'e2e-expr-insert-ref';
const CALC_GROUP_NAME = 'e2e-expr-insert-calc-group';
const COMPUTED_TAG_NAME = 'e2e-expr-insert-computed';
/**
 * #379 再レビュー対応: レジストリのタグ名検証は banto-expr の識別子文法より
 * 広いので、日本語名のタグは登録できるが式からは参照できない。この行は
 * 「一覧から挿入」で挿入せず理由を出す（テスト9）。
 */
const JP_TAG_NAME = 'e2e温度センサ';

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

/**
 * この spec が使う固定名（`CONNECTION_NAME`/`GROUP_NAME`/`CALC_GROUP_NAME`）の
 * PLC接続・収集グループ・配下タグを、存在すれば掃除する
 * （`banto-hub-tags-revision.spec.ts::cleanupExistingFixtures` を写したもの
 * - 流儀をそちらに揃えている）。
 *
 * 2つの理由で `beforeAll` の先頭と `afterAll` の両方で呼ぶ:
 * - `plc_connections`/`collection_groups` は `name` が UNIQUE
 *   （`crates/banto-tags/migrations/0001,0002`）なので、失敗テストのリトライで
 *   `beforeAll` が再走すると前回の同名リソースで UNIQUE 違反になり
 *   `beforeAll` ごと落ちる。
 * - スイート全体で1つの DB を共有しており、残したタグが後続スペックの
 *   一覧行数を増やす（`banto-hub-tags-revision.spec.ts` 冒頭の doc comment
 *   にある「仮想化されたグリッドの描画窓」を押し上げる）。
 *
 * FK は RESTRICT なので削除順は タグ → グループ → 接続。`calc` 予約接続は
 * 削除しない（サーバー起動時に自動作成される共有リソース）ので、その配下の
 * グループ（`CALC_GROUP_NAME`）とタグだけを消す。
 */
async function cleanupExistingFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (!groupsRes.ok()) return;
	const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
	const targetGroups = groups.filter((g) => g.name === GROUP_NAME || g.name === CALC_GROUP_NAME);

	if (targetGroups.length > 0) {
		const tagsRes = await request.get('/api/tags', { headers });
		if (tagsRes.ok()) {
			const tags = (await tagsRes.json()) as Array<{ id: number; collectionGroupId: number }>;
			const groupIds = new Set(targetGroups.map((g) => g.id));
			for (const tag of tags.filter((t) => groupIds.has(t.collectionGroupId))) {
				await request.delete(`/api/tags/${tag.id}`, { headers });
			}
		}
		for (const group of targetGroups) {
			await request.delete(`/api/collection-groups/${group.id}`, { headers });
		}
	}

	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as Array<{ id: number; name: string }>;
		const existing = connections.find((c) => c.name === CONNECTION_NAME);
		if (existing) {
			await request.delete(`/api/plc-connections/${existing.id}`, { headers });
		}
	}
}

/**
 * #379 レビュー対応3: 狭幅フォールバックの確認に使うビューポート。#375 の
 * `banto-hub-tags-edit-pane.spec.ts` と同じ 880x800（`mobileNavStore.isNarrow`
 * = `(max-width: 900px)` の内側で、かつグリッドを操作できる幅。同 spec の
 * `NARROW_VIEWPORT` の注記参照）。
 */
const NARROW_VIEWPORT = { width: 880, height: 800 };

test.describe.serial('banto-hub 演算タグの式欄「一覧から挿入」 (#342 段階C)', () => {
	let page: Page;
	/** #379 レビュー対応3: 狭幅テストで別タブへ流し込むため describe スコープに持つ。 */
	let token: string;
	/** `afterAll` の後始末でも使うため describe スコープに持つ。 */
	let authedHeaders: Record<string, string>;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 失敗テストのリトライで beforeAll が再走した場合に備え、前回分の
		// 同名リソースを先に掃除しておく（初回実行では何もしない）。
		await cleanupExistingFixtures(page.request, authedHeaders);

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

		// #379 再レビュー対応: 同じグループへ日本語名のタグも1件作る。
		// レジストリの名前検証は通る（空でない・最大長のみ）が、banto-expr の
		// 識別子文法では書けないので挿入候補から外れる（テスト9）。
		const jpTagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: JP_TAG_NAME,
				collectionGroupId: group.id,
				address: '40002',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(jpTagRes.ok()).toBe(true);

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
		// 後続スペックの一覧行数を増やさないよう、作ったフィクスチャは必ず
		// 片付ける（`cleanupExistingFixtures` の doc comment 参照）。
		await cleanupExistingFixtures(page.request, authedHeaders);
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
		// #379 レビュー対応4: 式欄のラベルは `<label for="tag-expression">` の
		// 明示形。`<label>` でフォーム全体を囲んでいた頃はトグルやエラー
		// メッセージ・プレビューのテキストまでアクセシブル名に混ざっていた。
		await expect(expressionField).toHaveAccessibleName(/^式（expression）/);
		await expect(expressionField).not.toHaveAccessibleName(/一覧から挿入/);

		const toggle = pane.getByTestId('tag-expression-insert-toggle');
		await expect(toggle).toBeVisible();
		await expect(toggle).toBeEnabled();
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

	test('6. 表編集モード中はトグルが押せず、ON のまま表編集へ入ると OFF になる（#379 レビュー対応1）', async () => {
		const pane = page.getByRole('complementary', { name: `${COMPUTED_TAG_NAME} を編集` });
		const toggle = pane.getByTestId('tag-expression-insert-toggle');
		const gridEditToggle = page.getByTestId('tag-grid-edit-mode-toggle');

		// まず ON にしてから表編集モードへ入ると、トグルは自動で OFF になり
		// 押せなくなる（BantoGrid が `editable` 列を持つ間はシングルクリックを
		// `onRowClick` へ流さないため - 押せるのに何も起きない状態を作らない）。
		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');

		await gridEditToggle.click();
		await expect(gridEditToggle).toHaveText('表編集を終了');
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');
		await expect(toggle).toBeDisabled();
		await expect(page.getByTestId('tag-insert-armed-badge')).toHaveCount(0);

		// 表編集モードを勝手に終了させることはしない（未保存セル編集の破棄確認を
		// 迂回しないため）- 明示的に終了させれば再び押せるようになる。
		await gridEditToggle.click();
		await expect(gridEditToggle).toHaveText('表編集');
		await expect(toggle).toBeEnabled();
	});

	test('6.5. 複数選択モード中もトグルが押せず、ON のまま複数選択へ入ると OFF になる（#379 レビュー対応）', async () => {
		const pane = page.getByRole('complementary', { name: `${COMPUTED_TAG_NAME} を編集` });
		const toggle = pane.getByTestId('tag-expression-insert-toggle');
		const selectionToggle = page.getByTestId('tag-selection-mode-toggle');

		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');

		// ON のまま「複数選択」へ入ると、トグルは自動で OFF になり押せなくなる
		// （`onRowClick` は `insertArmed` が最優先なので、両立させると
		// 「複数選択を終了」と出ているのに1行も選べない不整合になる）。
		await selectionToggle.click();
		await expect(selectionToggle).toHaveText('複数選択を終了');
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');
		await expect(toggle).toBeDisabled();
		await expect(page.getByTestId('tag-insert-armed-badge')).toHaveCount(0);

		// 選択モードの既存挙動が生きている（行クリックで選択が切り替わる -
		// 選択中の行は `rowClass` の `tag-row-selected` で強調される）。
		await page.getByRole('gridcell', { name: COMPUTED_TAG_NAME, exact: true }).click();
		await expect(page.locator('.row.tag-row-selected')).toHaveCount(1);

		// 選択モードを勝手に終了させることはしない（選択集合を黙って捨てない
		// ため）- 明示的に終了させれば再び押せるようになる。
		await selectionToggle.click();
		await expect(selectionToggle).toHaveText('複数選択');
		await expect(page.locator('.row.tag-row-selected')).toHaveCount(0);
		await expect(toggle).toBeEnabled();
	});

	test('7. ON のまま「新規登録」を押すと、モード遷移でトグルは OFF になる（#379 レビュー対応2）', async () => {
		const editPane = page.getByRole('complementary', { name: `${COMPUTED_TAG_NAME} を編集` });
		const editToggle = editPane.getByTestId('tag-expression-insert-toggle');
		await editToggle.click();
		await expect(editToggle).toHaveAttribute('aria-pressed', 'true');

		// `calc` グループが選択されたままなので、新規登録も `tagKind === 'computed'`
		// ＝トグルの表示条件は満たしたまま。それでもモード遷移で OFF に戻る。
		await page.getByRole('button', { name: '新規登録' }).click();

		const createPane = page.getByRole('complementary', { name: '新規作成' });
		await expect(createPane).toBeVisible();
		const createToggle = createPane.getByTestId('tag-expression-insert-toggle');
		await expect(createToggle).toBeVisible();
		await expect(createToggle).toHaveAttribute('aria-pressed', 'false');
		await expect(page.getByTestId('tag-insert-armed-badge')).toHaveCount(0);
	});

	test('8. 狭幅では新規作成が Modal で出て、「一覧から挿入」トグルは出ない（#379 レビュー対応3）', async ({
		browser
	}) => {
		// `sessionStorage` はタブごとなので、新しいページには改めてトークンを
		// 流し込む（`banto-hub-tags-edit-pane.spec.ts` の注記と同じ）。
		const narrowPage = await browser.newPage({ viewport: NARROW_VIEWPORT });
		try {
			await narrowPage.goto('/login');
			await injectAuthToken(narrowPage, token);
			await narrowPage.goto('/tags');

			await groupNodeByName(narrowPage, CALC_GROUP_NAME).click();
			await narrowPage.getByRole('button', { name: '新規登録' }).click();

			// 狭幅の新規作成は従来どおり中央モーダル（`role="dialog"`）。
			const modal = narrowPage.getByRole('dialog', { name: '新規作成' });
			await expect(modal).toBeVisible();
			await expect(narrowPage.getByRole('complementary', { name: '新規作成' })).toHaveCount(0);

			// `tagKind` は `calc` グループ由来で `computed` に確定しており式欄は
			// 出るが、オーバーレイの下のグリッドを触れないのでトグルは出さない。
			await expect(modal.getByLabel('式')).toBeVisible();
			await expect(modal.getByTestId('tag-expression-insert-toggle')).toHaveCount(0);
		} finally {
			await narrowPage.close();
		}
	});

	test('9. 式で表せない名前（日本語名）のタグは挿入されず、理由のトーストが出る（#379 再レビュー対応）', async () => {
		// テスト7で開いた新規作成ペイン（`tagKind === 'computed'`、式は空）を使う。
		const pane = page.getByRole('complementary', { name: '新規作成' });
		await expect(pane).toBeVisible();
		const expressionField = pane.getByLabel('式');
		await expect(expressionField).toHaveValue('');

		const toggle = pane.getByTestId('tag-expression-insert-toggle');
		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');

		// 日本語名タグのいる PLC グループへ絞る（ペインは非モーダルなので
		// そのまま操作できる）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('gridcell', { name: JP_TAG_NAME, exact: true }).click();

		// 式には何も入らず、理由がトーストで出る（編集対象も切り替わらない）。
		await expect(page.getByText('式で表せない名前のタグです')).toBeVisible();
		await expect(expressionField).toHaveValue('');
		await expect(pane).toBeVisible();

		// 同じグループの ASCII 名タグは従来どおり挿入できる（除外が広すぎない）。
		await page.getByRole('gridcell', { name: REF_TAG_NAME, exact: true }).click();
		await expect(expressionField).toHaveValue(REF_EXTERNAL_NAME);
	});
});
