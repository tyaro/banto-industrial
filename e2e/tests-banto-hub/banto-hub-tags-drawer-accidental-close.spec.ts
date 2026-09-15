/**
 * Drawer/Modal の誤爆クローズ防止（2026-09-15、オーナー報告「設定中に
 * 操作ミスで閉じてしまい最初からやり直しになる」）の実 DOM 受け入れテスト。
 *
 * `Drawer.svelte`/`Modal.svelte` は `dirty` prop が `true` の間、Esc と
 * オーバーレイクリックでは `requestClose()` を呼ばなくなった
 * （`drawerCloseGuard.test.ts` で純関数としては固定済み）。ここでは実
 * ブラウザで Esc キー押下・オーバーレイへの実クリックが本当に何も起こさ
 * ないこと、`×` 経由の確認（`window.confirm`）は従来どおり効くことを
 * 固定する。
 *
 * - 構造体登録 Drawer（`tags/+page.svelte`、`confirmDiscardIfNeeded`/
 *   `isDrawerDirty` は T18-1 で既存 - `banto-hub-tags-dirty-confirm.spec.ts`
 *   が `×` 経由の破棄確認は既に固定しているが、Esc・オーバーレイクリックの
 *   固定はまだ無かった）。
 *   **#375（2026-09-15）でタグ編集 Drawer から構造体登録 Drawer へ移した**:
 *   タグの「編集」「連続登録」は非モーダルの右ペインになり、オーバーレイも
 *   Esc クローズも持たなくなった（＝このテストの前提が消えた）ため、
 *   **モーダルのまま残る `struct`（構造体登録）を対象に付け替えた**。
 *   検証しているのは `tags/+page.svelte` 側の
 *   `dirty`/`onBlockedClose`/`confirmDiscardIfNeeded` の配線で、モードが
 *   違っても同じ1本の `<Drawer>` の同じ prop を通る（#376 の回帰ガード
 *   としての役目は変わらない）。非モーダルになったペイン側の挙動
 *   （Esc で閉じない・オーバーレイが無い）は
 *   `banto-hub-tags-edit-pane.spec.ts` が固定する。
 * - 接続 Drawer（`ConnectionDrawer.svelte`）: **今回まで未保存確認自体が
 *   無く**、Esc・オーバーレイクリックはおろか `×` でも確認なしに即閉じて
 *   いた経路。誤爆防止だけでなく確認そのものが新設されたことをテスト2本
 *   （Esc・オーバーレイ）で固定する。
 *
 * `banto-hub-tags-dirty-confirm.spec.ts` と同じパターン: 別
 * `describe.serial` ブロック（別 `page`）、認証・前提データは `page.request`
 * で直接 REST を叩く。ファイル名は `banto-hub-smoke.spec.ts` より辞書順で
 * 後（`banto-hub-auth.ts` の注記参照）。
 */
import { expect, test, type Dialog, type Locator, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-drawer-accidental-close-plc';
const GROUP_NAME = 'e2e-drawer-accidental-close-group';
const TAG_NAME = 'e2e-drawer-accidental-close-tag';

/**
 * `drawer`（`role="dialog"`）の直接の親であるオーバーレイ div
 * （`Drawer.svelte`/`Modal.svelte` の `role="presentation"`）へ実クリック
 * する。**`page.locator('[role="presentation"]')` は使わない** - BantoGrid
 * が自身の内部要素（スクロールコンテナ等）にも `role="presentation"` を
 * 付けており、strict mode 違反（複数要素マッチ）になる（実測）。左上隅は
 * Drawer（右寄せ）/Modal（中央）のどちらのパネルとも重ならない。
 */
async function clickOverlay(drawer: Locator): Promise<void> {
	await drawer.locator('xpath=..').click({ position: { x: 5, y: 5 } });
}

/**
 * Esc・オーバーレイクリックで `window.confirm` すら呼ばれないことを検査
 * するための一時的なダイアログ監視。**`stop()` を必ず呼ぶこと** -
 * `page.once('dialog', ...)` は「一度も発火しなければ解除されない」ため、
 * 後始末（`closeDirtyDrawerViaCloseButton` の正規の `window.confirm`）まで
 * この監視ハンドラが先に拾って `dismiss()` してしまい、後始末側の
 * `dialog.accept()` が「Cannot accept dialog which is already handled!」に
 * なる不具合を実測した（`page.on`/`page.off` で明示的に解除する）。
 */
function watchNoDialog(page: Page): { shown: () => boolean; stop: () => void } {
	let shown = false;
	const handler = (dialog: Dialog): void => {
		shown = true;
		void dialog.dismiss();
	};
	page.on('dialog', handler);
	return {
		shown: () => shown,
		stop: () => page.off('dialog', handler)
	};
}

/**
 * ブロックされたことを知らせる `onBlockedClose` トースト（`notifyBlockedClose`、
 * `tags/+page.svelte`/`ConnectionDrawer.svelte`/`CollectionGroupDrawer.svelte`
 * 共通の文言）が出たことを確認し、その場で消しておく。トーストの `×`
 * （`ToastHost.svelte`）は Drawer の `×`（`aria-label="閉じる"`）と同じ
 * アクセシブル名を持つため、`page.getByRole('status')` に限定して押す
 * - スコープしないと後続の操作で `名前 '閉じる'` ボタンが2つヒットし
 * strict mode 違反になる。
 */
async function expectBlockedCloseToastAndDismiss(page: Page): Promise<void> {
	const status = page.getByRole('status');
	await expect(
		status.getByText('未保存の変更があります。閉じるには × を押してください。')
	).toBeVisible();
	await status.getByRole('button', { name: '閉じる' }).click();
	await expect(
		status.getByText('未保存の変更があります。閉じるには × を押してください。')
	).toBeHidden();
}

/**
 * dirty な Drawer/Modal を `×` → `window.confirm` の OK 経由で後始末する
 * （各テストの最後に呼び、次のテストが綺麗な一覧状態から始められるように
 * する）。**このファイルでは `page.goto('/tags')` を各テストの直前で
 * 連打しない** - 実測で、直前のテストが未保存の Drawer を開いたまま残す
 * 状態から短時間に `page.goto` を連発すると `net::ERR_ABORTED`
 * （`about:blank` への goto ですら失敗する）になることを確認しており、
 * `/tags` への初回遷移は `test.beforeAll` で1回だけ行い、以降は同じ
 * ページ内の操作（DOM クリック）だけで各テストの前提状態を作り直す。
 */
async function closeDirtyDrawerViaCloseButton(page: Page, drawer: Locator): Promise<void> {
	page.once('dialog', (dialog) => {
		void dialog.accept();
	});
	await drawer.getByRole('button', { name: '閉じる' }).click();
	await expect(drawer).toBeHidden();
}

test.describe.serial('banto-hub Drawer/Modal 誤爆クローズ防止 (2026-09-15)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
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

		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: TAG_NAME,
				collectionGroupId: group.id,
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);

		// `/tags` への遷移はスイート全体でここ1回だけ（上の
		// `closeDirtyDrawerViaCloseButton` の doc comment参照）。以降の各
		// テストは DOM 操作だけで前提状態（Drawer が閉じた一覧表示）へ戻す。
		await page.goto('/tags');
	});

	test.afterAll(async () => {
		await page.close();
	});

	test.describe('構造体登録 Drawer（#375 でタグ編集 Drawer から付け替え）', () => {
		test.beforeEach(async () => {
			// 「構造体登録」ボタンはツリーで収集グループを選択している間だけ
			// 出る（`registrationTarget`、`tags/+page.svelte`）。
			await groupNodeByName(page, GROUP_NAME).click();
			await page.getByTestId('struct-reg-open').click();
			await expect(page.getByRole('dialog', { name: '構造体登録' })).toBeVisible();
		});

		test('a. 値を変更後 Esc を押しても閉じない（確認ダイアログも出ない）', async () => {
			const drawer = page.getByRole('dialog', { name: '構造体登録' });
			// ベースアドレスを入力すると `structBaseline` から外れて dirty になる
			// （`isDrawerDirty()` の `case 'struct'`）。
			const baseAddress = drawer.getByTestId('struct-reg-base-address');
			await baseAddress.fill('D4100');

			const dialogWatch = watchNoDialog(page);
			await page.keyboard.press('Escape');

			// dirty のため Esc はブロックされる - Drawer は開いたまま、入力も
			// 保持され、window.confirm すら呼ばれない（ブロック自体は確認より
			// 手前で起きる）。
			await expect(drawer).toBeVisible();
			await expect(baseAddress).toHaveValue('D4100');
			expect(dialogWatch.shown()).toBe(false);
			dialogWatch.stop();
			await expectBlockedCloseToastAndDismiss(page);

			await closeDirtyDrawerViaCloseButton(page, drawer);
		});

		test('b. 値を変更後オーバーレイをクリックしても閉じない', async () => {
			const drawer = page.getByRole('dialog', { name: '構造体登録' });
			const baseAddress = drawer.getByTestId('struct-reg-base-address');
			await baseAddress.fill('D4200');

			const dialogWatch = watchNoDialog(page);
			await clickOverlay(drawer);

			await expect(drawer).toBeVisible();
			await expect(baseAddress).toHaveValue('D4200');
			expect(dialogWatch.shown()).toBe(false);
			dialogWatch.stop();
			await expectBlockedCloseToastAndDismiss(page);

			await closeDirtyDrawerViaCloseButton(page, drawer);
		});

		test('c. 値を変更後 × → confirm が出る。キャンセルすると開いたまま、OK で閉じる', async () => {
			const drawer = page.getByRole('dialog', { name: '構造体登録' });
			const baseAddress = drawer.getByTestId('struct-reg-base-address');
			await baseAddress.fill('D4300');

			let dialogMessage: string | null = null;
			page.once('dialog', (dialog) => {
				dialogMessage = dialog.message();
				void dialog.dismiss();
			});
			await drawer.getByRole('button', { name: '閉じる' }).click();
			await expect
				.poll(() => dialogMessage, { message: 'window.confirm が呼ばれること（キャンセル）' })
				.toBe('変更を破棄しますか？');
			await expect(drawer).toBeVisible();
			await expect(baseAddress).toHaveValue('D4300');

			page.once('dialog', (dialog) => {
				void dialog.accept();
			});
			await drawer.getByRole('button', { name: '閉じる' }).click();
			await expect(drawer).toBeHidden();
		});
	});

	test.describe('接続 Drawer（今回まで未保存確認自体が無かった経路）', () => {
		test.beforeEach(async () => {
			// `simulation: true` の接続は `⚠ SIM` バッジが名前の直後に付く
			// （`ConnectionTree.svelte` の `⚠ SIM` バッジ、`connectionTreeBuild.ts`
			// 参照）ため、ここは `exact: true` にできない - 部分一致で足りる
			// （このテスト内で `CONNECTION_NAME` を含む他ノードは無い）。
			const connectionNode = page.getByRole('tree').getByRole('button', { name: CONNECTION_NAME });
			await connectionNode.click({ button: 'right' });
			await page.getByRole('menuitem', { name: '接続を再設定', exact: true }).click();
			await expect(page.getByRole('dialog', { name: `${CONNECTION_NAME} を編集` })).toBeVisible();
		});

		test('a. 名前を変更後 Esc を押しても閉じない', async () => {
			const drawer = page.getByRole('dialog', { name: `${CONNECTION_NAME} を編集` });
			const nameInput = drawer.getByLabel('名前');
			await nameInput.fill(`${CONNECTION_NAME}-esc`);

			const dialogWatch = watchNoDialog(page);
			await page.keyboard.press('Escape');

			await expect(drawer).toBeVisible();
			await expect(nameInput).toHaveValue(`${CONNECTION_NAME}-esc`);
			expect(dialogWatch.shown()).toBe(false);
			dialogWatch.stop();
			await expectBlockedCloseToastAndDismiss(page);

			await closeDirtyDrawerViaCloseButton(page, drawer);
		});

		test('b. 名前を変更後オーバーレイをクリックしても閉じない', async () => {
			const drawer = page.getByRole('dialog', { name: `${CONNECTION_NAME} を編集` });
			const nameInput = drawer.getByLabel('名前');
			await nameInput.fill(`${CONNECTION_NAME}-overlay`);

			const dialogWatch = watchNoDialog(page);
			await clickOverlay(drawer);

			await expect(drawer).toBeVisible();
			await expect(nameInput).toHaveValue(`${CONNECTION_NAME}-overlay`);
			expect(dialogWatch.shown()).toBe(false);
			dialogWatch.stop();
			await expectBlockedCloseToastAndDismiss(page);

			await closeDirtyDrawerViaCloseButton(page, drawer);
		});
	});
});
