/**
 * #375（2026-09-15 オーナー決定、docs/banto-hub-desktop-plan.md §9.4
 * TAG-UX-C 追補）: タグの「編集」「連続登録」を非モーダルの右ペインへ移した
 * ことの実 DOM 受け入れテスト。
 *
 * このファイルを新設した理由: 既存のタグ編集系スペック
 * （`banto-hub-tags-dirty-confirm.spec.ts` 等）はロケータを
 * `role="dialog"` → `role="complementary"` に付け替えただけで、
 * **「非モーダルであること」そのもの**は誰も固定していない。モーダルへ
 * 戻す実装（オーバーレイの復活、Esc クローズの復活）を回帰として検出できる
 * ようにするのがここの責務:
 *
 * 1. タグ行クリックで右ペインが開き、**同時に左ツリーがクリックできる**
 *    （オーバーレイに覆われていない - 覆われていれば Playwright の
 *    actionability チェックがタイムアウトする）。
 * 2. ペインを開いたままグリッドの別行をクリックすると、未保存なら従来どおり
 *    確認が出る（`selectTag` → `confirmDiscardIfNeeded`、#375 で変えない
 *    と決めた挙動）。
 * 3. **Esc を押してもペインは閉じない。**「閉じる」ボタンで閉じる。
 * 4. 狭幅（`banto-hub-viewport-offcanvas.spec.ts` と同じブレークポイント
 *    `mobileNavStore.isNarrow` = `(max-width: 900px)`）では従来どおり
 *    オーバーレイの Drawer で出る。
 *
 * ファイル名は `banto-hub-smoke.spec.ts` より辞書順で後（`banto-hub-auth.ts`
 * の注記参照）。前提データは UI ではなく `page.request` で直接 REST を叩いて
 * 作る（`banto-hub-tags-dirty-confirm.spec.ts` と同じパターン）。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-edit-pane-plc';
const GROUP_NAME = 'e2e-edit-pane-group';
const TAG_A = 'e2e-edit-pane-tag-a';
const TAG_B = 'e2e-edit-pane-tag-b';

/**
 * 狭幅フォールバックの判定は `mobileNavStore.isNarrow`
 * （`NARROW_BREAKPOINT_QUERY = '(max-width: 900px)'`）で、
 * `banto-hub-viewport-offcanvas.spec.ts` のオフキャンバス退避と**同じ
 * ブレークポイント**（新しい値は発明していない）。
 *
 * ただしビューポート幅そのものは同スペックの 400px ではなく 880px を使う:
 * 400px だと左ツリーの固定幅 280px を引いた残り ≈120px にグリッドが
 * 押し込まれ、`.content { overflow: hidden }` で clip された行が
 * Playwright の actionability チェックを通らない（実測でクリックが
 * タイムアウトする）。狭幅でのグリッド最適化自体は TAG-UX-G のバックログで、
 * ここで検証したいのは「同じ `(max-width: 900px)` の内側なら Drawer へ
 * フォールバックするか」なので、同じメディアクエリに収まる中でグリッドを
 * 操作できる幅を選ぶ。
 */
const NARROW_VIEWPORT = { width: 880, height: 800 };

test.describe.serial('banto-hub タグ編集の非モーダル右ペイン (#375)', () => {
	let page: Page;
	let token: string;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

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

		// modbus-tcp 接続配下なので Modbus 参照番号形式のアドレス
		// （`banto-hub-tags-dirty-confirm.spec.ts` の注記と同じ理由）。
		for (const [name, address] of [
			[TAG_A, '40101'],
			[TAG_B, '40102']
		] as const) {
			const tagRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: {
					name,
					collectionGroupId: group.id,
					address,
					dataType: 'i16',
					decimals: 0,
					enabled: true,
					writable: false,
					tagKind: 'plc'
				}
			});
			expect(tagRes.ok()).toBe(true);
		}
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 行クリックで右ペインが開き、覆われずに左ツリーをそのままクリックできる', async () => {
		await page.goto('/tags');
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();

		const pane = page.getByRole('complementary', { name: `${TAG_A} を編集` });
		await expect(pane).toBeVisible();

		// ここが非モーダル化の本体: モーダルなら `Drawer.svelte` の
		// `.overlay`（`position: fixed; inset: 0`）がツリーを覆うため、この
		// クリックは actionability チェックでタイムアウトする。
		await groupNodeByName(page, GROUP_NAME).click();

		// ツリー選択が実際に効いていること（ツールバーの登録先表示が
		// 対象グループに変わる）。
		await expect(page.getByTestId('tag-registration-target')).toContainText(GROUP_NAME);
		// ツリーを触ってもペインは閉じない。
		await expect(pane).toBeVisible();
	});

	test('2. ペインを開いたままグリッドの別行を選ぶと、未保存なら確認が出る（現行挙動の維持）', async () => {
		const pane = page.getByRole('complementary', { name: `${TAG_A} を編集` });
		await expect(pane).toBeVisible();

		// BantoGrid の列フィルターボタン（aria-label="名前の絞り込み"）と
		// `getByLabel('名前')` が重複するため、ペイン内に限定する。
		await pane.getByLabel('名前').fill(`${TAG_A}-dirty`);

		let dialogMessage: string | null = null;
		page.once('dialog', (dialog) => {
			dialogMessage = dialog.message();
			void dialog.dismiss();
		});
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click();
		await expect
			.poll(() => dialogMessage, { message: 'window.confirm が呼ばれること（キャンセル）' })
			.toBe('変更を破棄しますか？');

		// キャンセルしたので対象は切り替わらず、入力も残る。
		await expect(pane).toBeVisible();
		await expect(pane.getByLabel('名前')).toHaveValue(`${TAG_A}-dirty`);

		// OK すれば従来どおり切り替わる（3択にはしない - #375 の決定）。
		page.once('dialog', (dialog) => {
			void dialog.accept();
		});
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click();
		await expect(page.getByRole('complementary', { name: `${TAG_B} を編集` })).toBeVisible();
		await expect(pane).toBeHidden();
	});

	test('3. Esc ではペインは閉じない。「閉じる」ボタンで閉じる', async () => {
		const pane = page.getByRole('complementary', { name: `${TAG_B} を編集` });
		await expect(pane).toBeVisible();

		// 未保存が無い状態でも Esc は効かない（モーダルではないため - Drawer と
		// 違い window の keydown ハンドラ自体を持たない）。
		await page.keyboard.press('Escape');
		await expect(pane).toBeVisible();

		// 値を変えて dirty にしても、Esc では当然閉じない。
		await pane.getByLabel('単位').fill('℃');
		await page.keyboard.press('Escape');
		await expect(pane).toBeVisible();
		await expect(pane.getByLabel('単位')).toHaveValue('℃');

		// 閉じるのは明示的な「閉じる」ボタン（未保存なら従来どおり確認）。
		page.once('dialog', (dialog) => {
			void dialog.accept();
		});
		await pane.getByRole('button', { name: '閉じる' }).click();
		await expect(pane).toBeHidden();
	});

	test('4. 狭幅では従来どおりオーバーレイの Drawer で出る', async ({ browser }) => {
		// `sessionStorage` はタブ（ブラウジングコンテキスト）ごとなので、
		// 新しいページには改めてトークンを流し込む（`injectAuthToken` の注記）。
		const narrowPage = await browser.newPage({ viewport: NARROW_VIEWPORT });
		try {
			await narrowPage.goto('/login');
			await injectAuthToken(narrowPage, token);
			await narrowPage.goto('/tags');

			await narrowPage.getByRole('gridcell', { name: TAG_A, exact: true }).click();

			// 狭幅では `<Drawer>`（`role="dialog"` + オーバーレイ）へフォールバック
			// するので、非モーダルのペインは存在しない。
			await expect(narrowPage.getByRole('dialog', { name: `${TAG_A} を編集` })).toBeVisible();
			await expect(narrowPage.getByRole('complementary', { name: `${TAG_A} を編集` })).toHaveCount(
				0
			);
		} finally {
			await narrowPage.close();
		}
	});
});
