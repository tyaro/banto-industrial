/**
 * #342 段階B（docs/tag-server-design.md §4.2「セグメント補完」）: 演算タグの
 * 式欄のセグメント補完の実 DOM 受け入れテスト。
 *
 * このファイルを新設した理由: 段階A の
 * `banto-hub-tags-expression-check.spec.ts` は式欄の**チェック結果表示**、
 * 段階C の `banto-hub-tags-expression-insert.spec.ts` は**一覧からの挿入**を
 * 見ており、段階B の本題（打鍵に追従して候補が出る・確定すると次の階層が
 * 開く・Ctrl+. で任意位置から開ける・**Esc はポップアップだけを閉じて
 * 「一覧から挿入」トグルは OFF にしない**・関数候補）は誰も固定していない。
 *
 * ファイル名は `banto-hub-smoke.spec.ts` より辞書順で後
 * （`banto-hub-auth.ts` の注記参照）で、段階A の `...-expression-check` と
 * 段階C の `...-expression-insert` の**あいだ**に並ぶ（`ch` < `co` < `in`）。
 * 3段階が並んで読めるようにするための命名。
 *
 * **グリッドの行は一切クリックしない**（補完はキーボードだけで完結する）ので、
 * `BantoGrid` の行仮想化（`banto-hub-tags-revision.spec.ts` 冒頭の doc
 * comment 参照）の影響を受けない。ツリーのノードだけを使う。
 *
 * **式欄の locator は `getByLabel('式')` ではなく
 * `getByRole('textbox', { name: /^式（expression）/ })`**（#380 レビュー対応C）:
 * 式欄を包むラッパーが `role="combobox"` + `aria-labelledby` で**同じラベルを
 * 共有する**ようになったため、`getByLabel` は combobox と textbox の2つに
 * 一致して strict mode 違反になる（これは ARIA として正しい形なので、locator
 * 側をロールで絞る）。段階A・段階C の spec も同じ理由で揃えてある。
 *
 * 前提データは UI ではなく `page.request` で直接 REST を叩いて作る
 * （`banto-hub-tags-expression-insert.spec.ts` と同じパターン）。接続名・
 * グループ名・タグ名は banto-expr の識別子文法（ASCII・英字始まり）を満たし、
 * かつ他スペックの固定名と衝突しないものにしてある（`plc_connections`/
 * `collection_groups` の `name` は UNIQUE）。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';
import { cleanupFixtures } from './banto-hub-fixture-cleanup';

const CONNECTION_NAME = 'e2ecompline1';
const GROUP_NAME = 'e2ecompfast';
const REF_TAG_NAME = 'e2ecomptemp';
/** `e2ecomptemp` と前方一致で区別できる2件目（絞り込みの確認に使う）。 */
const OTHER_TAG_NAME = 'e2ecompother';
const CALC_GROUP_NAME = 'e2e-expr-comp-calc-group';

const REF_EXTERNAL_NAME = `${CONNECTION_NAME}.${GROUP_NAME}.${REF_TAG_NAME}`;

/**
 * 掃除の対象（共有ヘルパー `banto-hub-fixture-cleanup.ts` に渡す）。`calc` 予約
 * 接続自体は消さず、その配下のグループとタグだけを消す。**`beforeAll` の先頭と
 * `afterAll` の両方**で呼ぶ理由と、依存元まで拾う理由はヘルパー側の doc comment 参照。
 */
const CLEANUP_TARGET = {
	groupNames: [GROUP_NAME, CALC_GROUP_NAME],
	connectionNames: [CONNECTION_NAME]
};

function waitForExpressionCheck(page: Page) {
	return page.waitForResponse(
		(res) =>
			res.url().includes('/api/tags/expression/check') &&
			res.request().method() === 'POST' &&
			res.status() === 200
	);
}

test.describe.serial('banto-hub 演算タグの式欄セグメント補完 (#342 段階B)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await cleanupFixtures(page.request, authedHeaders, CLEANUP_TARGET);

		// 前提データ1: シミュレーションモードの PLC接続 + 収集グループ +
		// 補完で選ぶ PLC タグ2件（実 PLC/実ネットワークへは繋がない）。
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
			data: { name: GROUP_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };

		for (const [index, name] of [REF_TAG_NAME, OTHER_TAG_NAME].entries()) {
			const tagRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: {
					name,
					collectionGroupId: group.id,
					address: String(40001 + index),
					dataType: 'i16',
					unit: '℃',
					decimals: 0,
					enabled: true,
					writable: false,
					tagKind: 'plc'
				}
			});
			expect(tagRes.ok()).toBe(true);
		}

		// 前提データ2: `calc` 予約接続（起動時に自動作成済み）配下の収集
		// グループ - ここから「新規登録」すると `tagKind` が `computed` に
		// 確定し、式欄が最初から出る（`tagOnboarding.ts::resolveRegistrationTarget`）。
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
		await cleanupFixtures(page.request, authedHeaders, CLEANUP_TARGET);

		// 掃除が実際に効いたことをサーバー側で確認する（削除順や preflight
		// 拒否で残っていれば、ここで落ちる）。
		const tagsRes = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(tagsRes.ok()).toBe(true);
		const remaining = ((await tagsRes.json()) as Array<{ name: string }>)
			.map((t) => t.name)
			.filter((name) => name.startsWith('e2ecomp'));
		expect(remaining, 'この spec のタグが残っています').toEqual([]);

		await page.close();
	});

	test('1. 接続名 + ドットで収集グループの候補が出て、確定すると次の階層まで入る', async () => {
		await page.goto('/tags');
		await groupNodeByName(page, CALC_GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();

		// 広幅では新規作成も非モーダルの右ペイン（#375 / #342 段階C）。
		const pane = page.getByRole('complementary', { name: '新規作成' });
		await expect(pane).toBeVisible();

		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		await expect(expressionField).toBeVisible();

		// 「接続名 + ドット」まで打つと第2セグメント（収集グループ）の候補が出る。
		await expressionField.fill(`${CONNECTION_NAME}.`);
		const popup = page.getByTestId('expression-completion');
		await expect(popup).toBeVisible();
		await expect(popup.getByRole('option', { name: new RegExp(GROUP_NAME) })).toBeVisible();

		// 確定（Enter）すると **名前 + ドット**が入り、そのまま第3セグメント
		// （タグ）の候補が開く。
		await page.keyboard.press('Enter');
		await expect(expressionField).toHaveValue(`${CONNECTION_NAME}.${GROUP_NAME}.`);
		await expect(popup).toBeVisible();
		await expect(popup.getByRole('option', { name: new RegExp(REF_TAG_NAME) })).toBeVisible();
		await expect(popup.getByRole('option', { name: new RegExp(OTHER_TAG_NAME) })).toBeVisible();
	});

	test('2. 3セグメント目を確定すると式チェックが走り、プレビューに参照タグが出る', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		const popup = page.getByTestId('expression-completion');

		// 打鍵で候補を絞る（`e2ecomptemp` だけが残る）。
		await expressionField.pressSequentially(REF_TAG_NAME.slice(0, 8));
		await expect(popup.getByRole('option')).toHaveCount(1);

		const checkResponse = waitForExpressionCheck(page);
		await page.keyboard.press('Enter');

		// タグを確定したらポップアップは閉じ、参照が完成する。
		await expect(popup).toHaveCount(0);
		await expect(expressionField).toHaveValue(REF_EXTERNAL_NAME);

		// 挿入が `input` として流れ、段階A のチェックが走ってプレビューに出る。
		await checkResponse;
		const preview = pane.getByTestId('expression-preview');
		await expect(preview).toBeVisible();
		await expect(preview).toContainText(REF_EXTERNAL_NAME);
	});

	test('3. Ctrl+. と Ctrl+Space のどちらでも任意の位置から補完を開ける', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		const popup = page.getByTestId('expression-completion');

		// 演算子の直後（前方一致0文字）は自動では開かない位置。
		await expressionField.fill('1 + ');
		await expect(popup).toHaveCount(0);

		await page.keyboard.press('Control+Period');
		await expect(popup).toBeVisible();

		// #380 レビュー対応G: **前方一致0文字のまま候補の中身を見ない**。候補は
		// 20件で打ち切られるので、スイート全体で共有している DB に表現可能な接続が
		// 20個あると落ちる（グリッド仮想化のときと同じ「共有 DB は増える」問題）。
		// 開いたことを確認したうえで、この spec 固有の prefix を打って絞ってから
		// 候補を見る。
		await expressionField.pressSequentially(CONNECTION_NAME.slice(0, 6));
		await expect(popup.getByRole('option', { name: new RegExp(CONNECTION_NAME) })).toBeVisible();

		// #380 レビュー対応E: `Ctrl+Space`（`event.code === 'Space'` の経路）でも
		// 開く - 受け入れ条件に両方入っている。Escape で閉じた直後でも、明示
		// トリガーは「明示的に閉じた後は自動で開かない」抑止を無視する
		// （#380 レビュー対応A）。
		await page.keyboard.press('Escape');
		await expect(popup).toHaveCount(0);
		await page.keyboard.press('Control+Space');
		await expect(popup).toBeVisible();
		await expect(popup.getByRole('option', { name: new RegExp(CONNECTION_NAME) })).toBeVisible();
	});

	test('4. Esc はポップアップだけを閉じ、「一覧から挿入」トグルは ON のまま', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		const popup = page.getByTestId('expression-completion');
		const toggle = pane.getByTestId('tag-expression-insert-toggle');

		// 段階C のトグルを ON にしてから補完を開く。
		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');

		await expressionField.click();
		await page.keyboard.press('Control+Period');
		await expect(popup).toBeVisible();

		// 1回目の Esc: ポップアップだけ閉じる（トグルは ON のまま）。
		await page.keyboard.press('Escape');
		await expect(popup).toHaveCount(0);
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');
		await expect(page.getByTestId('tag-insert-armed-badge')).toBeVisible();

		// 2回目の Esc: 段階C の既存挙動どおりトグルが OFF になる（ペインは閉じない）。
		await page.keyboard.press('Escape');
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');
		await expect(page.getByTestId('tag-insert-armed-badge')).toHaveCount(0);
		await expect(pane).toBeVisible();
	});

	test('5. 組み込み関数の候補を確定すると `name(` が入る', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		const popup = page.getByTestId('expression-completion');

		// 2文字以上の前方一致で自動的に開く。関数表はサーバー
		// （`GET /api/tags/expression/functions`）から配られたもの。
		await expressionField.fill('mi');
		await expect(popup).toBeVisible();
		await expect(popup.getByRole('option')).toHaveCount(1);
		await expect(popup.getByRole('option', { name: /min/ })).toBeVisible();

		await page.keyboard.press('Enter');
		await expect(popup).toHaveCount(0);
		await expect(expressionField).toHaveValue('min(');
	});

	test('5.5. 候補をクリックしても確定でき、フォーカスは式欄に残る（#380 レビュー対応2）', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		const popup = page.getByTestId('expression-completion');

		// 確定経路のうち Enter/Tab は他のテストで見ているが、**クリック**は
		// `CompletionPopup.svelte` の `onmousedown` + `preventDefault()`（式欄から
		// フォーカスを外させず `selectionStart` を保つための肝）に依存するので、
		// ここで別途固定する。回帰しても Enter/Tab のテストは緑のままになるため。
		await expressionField.fill(CONNECTION_NAME.slice(0, 6));
		await expect(popup).toBeVisible();
		await popup.getByRole('option', { name: new RegExp(CONNECTION_NAME) }).click();

		// 接続名の確定なので「名前 + ドット」が入り、次の階層が開く。
		await expect(expressionField).toHaveValue(`${CONNECTION_NAME}.`);
		// フォーカスが式欄に残っている（＝キャレット位置を失っていない）。
		await expect(expressionField).toBeFocused();
		const selectionStart = await expressionField.evaluate(
			(el: HTMLTextAreaElement) => el.selectionStart
		);
		expect(selectionStart).toBe(CONNECTION_NAME.length + 1);
		await expect(popup).toBeVisible();
		await expect(popup.getByRole('option', { name: new RegExp(GROUP_NAME) })).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(popup).toHaveCount(0);
	});

	test('6. スクロールした長い式でもポップアップが式欄の可視範囲に出る（#380 レビュー対応1）', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		const popup = page.getByTestId('expression-completion');

		// 式欄は `rows="2"` なので、改行を並べれば幅に依存せず確実にスクロール
		// させられる（キャレット座標を測るミラーは `overflow: hidden` で
		// スクロールしないため、textarea の `scrollTop` を引かないとポップアップが
		// 「スクロールしていないときの行位置」＝式欄のはるか下に出てしまう）。
		await expressionField.fill(`${'\n'.repeat(12)}mi`);
		await expressionField.evaluate((el: HTMLTextAreaElement) => {
			el.setSelectionRange(el.value.length, el.value.length);
			el.scrollTop = el.scrollHeight;
		});

		await page.keyboard.press('Control+Period');
		await expect(popup).toBeVisible();

		const fieldBox = await expressionField.boundingBox();
		const popupBox = await popup.boundingBox();
		expect(fieldBox).not.toBeNull();
		expect(popupBox).not.toBeNull();
		// 厳密な座標は見ない（フォント・行高に依存する）。「式欄の矩形から大きく
		// 外れていない」ことだけを固定する - 修正前はスクロール量（12行ぶん）だけ
		// 下にずれるので、この緩い範囲でも確実に落ちる。
		expect(popupBox!.y).toBeGreaterThan(fieldBox!.y - 120);
		expect(popupBox!.y).toBeLessThan(fieldBox!.y + fieldBox!.height + 80);

		// **式欄自身のスクロールで閉じない**こと（2026-09-16 の CI 失敗で判明した
		// 実挙動の不具合の回帰固定）。textarea は `rows="2"` なので長い式を打つと
		// 編集のたびに自動スクロールする - それを「ページがスクロールした」と同じ
		// 扱いで閉じていたため、長い式ほど候補が消えていた。いまは座標を取り直す
		// だけなので開いたまま、位置も式欄の近くに保たれる。
		await expressionField.evaluate((el: HTMLTextAreaElement) => {
			el.scrollTop = 0;
		});
		// 見るのは「**閉じていない**」ことだけ。ここでは先頭までスクロールを戻して
		// いるのでキャレット行自体が式欄の外へ出ており、取り直した座標が式欄の
		// 矩形から離れるのは正しい（実運用の自動スクロールはキャレットを見える位置へ
		// 動かすので、そちらでは座標も式欄の近くに保たれる）。
		await page.waitForTimeout(100);
		await expect(popup).toBeVisible();
	});

	test('7. 補完を開いたままペインを閉じるとポップアップも消える（#380 レビュー対応2）', async () => {
		const pane = page.getByRole('complementary', { name: '新規作成' });
		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		const popup = page.getByTestId('expression-completion');

		// テスト6 が式欄をスクロールさせたままなので、まず空にしてスクロール位置を
		// 戻す（`scroll` が飛ぶと #380 レビュー対応3 の「スクロールで閉じる」が
		// 働くため、補完を開くのはスクロールが落ち着いてから）。
		await expressionField.fill('');
		await expressionField.fill('mi');
		await expect(popup).toBeVisible();

		// ペインを閉じる（未保存なので破棄確認が挟まる）。補完の状態はページ直下に
		// あるので、モード遷移で明示的にリセットしないと古い候補が残る。
		page.once('dialog', (dialog) => {
			void dialog.accept();
		});
		// `exact: true` は必須 - ペインには「登録して閉じる」ボタンもあるため。
		await pane.getByRole('button', { name: '閉じる', exact: true }).click();

		await expect(pane).toHaveCount(0);
		await expect(popup).toHaveCount(0);
	});

	test('8. IME 変換の確定後に補完が出る（#380 レビュー対応1）', async () => {
		// テスト7でペインを閉じたので開き直す。
		await groupNodeByName(page, CALC_GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		const pane = page.getByRole('complementary', { name: '新規作成' });
		await expect(pane).toBeVisible();
		const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
		const popup = page.getByTestId('expression-completion');
		await expect(popup).toHaveCount(0);

		// Playwright の `type`/`insertText` では IME の composition を再現できない
		// ので、**問題になっていたブラウザ挙動そのもの**を合成イベントで再現する:
		// 確定文字列ぶんの `input` が composition 中（`isComposing: true`）に発火し、
		// `compositionend` の後には `input` が来ない、という順序。修正前はこの順序で
		// 補完が閉じたまま二度と開かなかった（`compositionend` でフラグを戻すだけで
		// 再評価していなかったため）。
		await expressionField.evaluate((el: HTMLTextAreaElement) => {
			el.focus();
			el.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
			el.value = 'mi';
			el.setSelectionRange(2, 2);
			el.dispatchEvent(new InputEvent('input', { bubbles: true, isComposing: true }));
		});

		// #380 レビュー対応F: **変換中は開かない**ことを先に確認する（これを見ずに
		// 確定後だけ見ると、変換中に開く実装でもテストが通ってしまう）。
		await expect(popup).toHaveCount(0);

		await expressionField.evaluate((el: HTMLTextAreaElement) => {
			el.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: 'mi' }));
		});

		await expect(popup).toBeVisible();
		await expect(popup.getByRole('option', { name: /min/ })).toBeVisible();
	});

	test('9. Escape で閉じた後に関数表のフェッチが解決しても開き直さない（#380 レビュー対応A）', async () => {
		// 関数表の応答をわざと遅らせる。ページを読み込み直すのは、この spec が
		// 共有している `page` では関数表が既に取得済み（キャッシュ済み）だから。
		const FUNCTIONS_URL = '**/api/tags/expression/functions';
		await page.route(FUNCTIONS_URL, async (route) => {
			await new Promise((resolve) => setTimeout(resolve, 1500));
			await route.continue();
		});
		try {
			const functionsResponse = page.waitForResponse((res) =>
				res.url().includes('/api/tags/expression/functions')
			);

			await page.goto('/tags');
			await groupNodeByName(page, CALC_GROUP_NAME).click();
			await page.getByRole('button', { name: '新規登録' }).click();
			const pane = page.getByRole('complementary', { name: '新規作成' });
			await expect(pane).toBeVisible();
			const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
			const popup = page.getByTestId('expression-completion');

			// フェッチが返る前に、タグ候補だけで補完を開く。
			await expressionField.fill(CONNECTION_NAME.slice(0, 4));
			await expect(popup).toBeVisible();

			// ユーザーが明示的に閉じる。
			await page.keyboard.press('Escape');
			await expect(popup).toHaveCount(0);

			// 遅れて関数表が届いても、勝手に開き直さない（Escape の約束を守る）。
			await functionsResponse;
			await expect(popup).toHaveCount(0);

			// 次の打鍵からは通常どおり開く（抑止は「次の input まで」）。続けて
			// 接続名の次の1文字を打つので、前方一致は保たれる。
			await expressionField.pressSequentially(CONNECTION_NAME.charAt(4));
			await expect(popup).toBeVisible();
		} finally {
			await page.unroute(FUNCTIONS_URL);
		}
	});

	test('9.5. カーソル移動で閉じた後も、遅れて届く関数表で開き直さない（#380 レビュー対応1）', async () => {
		// テスト9 と同じ「応答を遅らせる」型を流用する。矢印キー等での close も
		// `dismissCompletion` にしていないと、フェッチ解決時に同じ文脈が再評価
		// されて開き直してしまう（「打ち直せば開く」と食い違う）。
		const FUNCTIONS_URL = '**/api/tags/expression/functions';
		await page.route(FUNCTIONS_URL, async (route) => {
			await new Promise((resolve) => setTimeout(resolve, 1500));
			await route.continue();
		});
		try {
			const functionsResponse = page.waitForResponse((res) =>
				res.url().includes('/api/tags/expression/functions')
			);

			await page.goto('/tags');
			await groupNodeByName(page, CALC_GROUP_NAME).click();
			await page.getByRole('button', { name: '新規登録' }).click();
			const pane = page.getByRole('complementary', { name: '新規作成' });
			await expect(pane).toBeVisible();
			const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
			const popup = page.getByTestId('expression-completion');

			await expressionField.fill(CONNECTION_NAME.slice(0, 4));
			await expect(popup).toBeVisible();

			// Escape ではなく**カーソル移動**で閉じる。
			await page.keyboard.press('Home');
			await expect(popup).toHaveCount(0);

			await functionsResponse;
			await expect(popup).toHaveCount(0);
		} finally {
			await page.unroute(FUNCTIONS_URL);
		}
	});

	test('10. 関数表の取得に失敗してもタグ候補は動き、式欄を開き直すと取り直す（#380 レビュー対応D）', async () => {
		const FUNCTIONS_URL = '**/api/tags/expression/functions';
		await page.route(FUNCTIONS_URL, (route) => route.abort());
		let routed = true;
		try {
			await page.goto('/tags');
			await groupNodeByName(page, CALC_GROUP_NAME).click();
			await page.getByRole('button', { name: '新規登録' }).click();
			const pane = page.getByRole('complementary', { name: '新規作成' });
			await expect(pane).toBeVisible();
			const expressionField = pane.getByRole('textbox', { name: /^式（expression）/ });
			const popup = page.getByTestId('expression-completion');

			// タグ候補（接続 → 収集グループ）は関数表と無関係に動く。
			await expressionField.fill(`${CONNECTION_NAME}.`);
			await expect(popup).toBeVisible();
			await expect(popup.getByRole('option', { name: new RegExp(GROUP_NAME) })).toBeVisible();

			// 関数候補だけが出ない（`mi` に前方一致する接続は無いので候補0件）。
			await expressionField.fill('mi');
			await expect(popup).toHaveCount(0);

			// 取得できる状態に戻し、式欄を開き直すと取り直す
			// （失敗時に `expressionFunctionsRequested` を戻しているため）。
			await page.unroute(FUNCTIONS_URL);
			routed = false;
			page.once('dialog', (dialog) => {
				void dialog.accept();
			});
			await pane.getByRole('button', { name: '閉じる', exact: true }).click();
			await expect(pane).toHaveCount(0);

			await page.getByRole('button', { name: '新規登録' }).click();
			const reopened = page.getByRole('complementary', { name: '新規作成' });
			await expect(reopened).toBeVisible();
			await reopened.getByRole('textbox', { name: /^式（expression）/ }).fill('mi');
			await expect(popup).toBeVisible();
			await expect(popup.getByRole('option', { name: /min/ })).toBeVisible();
		} finally {
			if (routed) await page.unroute(FUNCTIONS_URL);
		}
	});

	test('11. 取得に成功した関数表は、式欄を開き直しても再取得しない（#380 レビュー対応10）', async () => {
		// 関数表は静的（サーバー側で DB も設定も読まない）なので、1回取れたら
		// キャッシュしたまま使う。以前は式欄が消えるたびに要求済みフラグを戻して
		// いたため、computed フォームを閉じて開くたびに取り直していた。
		let requests = 0;
		const countFunctionsRequest = (request: { url: () => string }): void => {
			if (request.url().includes('/api/tags/expression/functions')) requests += 1;
		};
		page.on('request', countFunctionsRequest);
		try {
			await page.goto('/tags');
			await groupNodeByName(page, CALC_GROUP_NAME).click();
			await page.getByRole('button', { name: '新規登録' }).click();
			const pane = page.getByRole('complementary', { name: '新規作成' });
			await expect(pane).toBeVisible();
			const popup = page.getByTestId('expression-completion');

			// 関数候補が出た = 取得できた。
			await pane.getByRole('textbox', { name: /^式（expression）/ }).fill('mi');
			await expect(popup.getByRole('option', { name: /min/ })).toBeVisible();
			expect(requests).toBe(1);

			// 閉じて開き直しても、関数候補はキャッシュから出る（往復は増えない）。
			page.once('dialog', (dialog) => {
				void dialog.accept();
			});
			await pane.getByRole('button', { name: '閉じる', exact: true }).click();
			await expect(pane).toHaveCount(0);

			await page.getByRole('button', { name: '新規登録' }).click();
			const reopened = page.getByRole('complementary', { name: '新規作成' });
			await expect(reopened).toBeVisible();
			await reopened.getByRole('textbox', { name: /^式（expression）/ }).fill('mi');
			await expect(popup.getByRole('option', { name: /min/ })).toBeVisible();
			expect(requests).toBe(1);
		} finally {
			page.off('request', countFunctionsRequest);
		}
	});
});
