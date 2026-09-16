/**
 * #378（2026-09-16 オーナー決定、docs/banto-hub-desktop-plan.md §9.4
 * TAG-UX-G）: 狭幅でタグ登録・タグモニタの左ツリーをオフキャンバスへ退避する
 * ことの実 DOM 受け入れテスト。
 *
 * このファイルを新設した理由: 400px 幅では固定 280px の左ツリーにグリッドが
 * 押されて残り ≈120px しか無く、`.content { overflow: hidden }` で clip された
 * 行が Playwright の actionability チェックを通らなかった（#375 の
 * `banto-hub-tags-edit-pane.spec.ts` がそのために狭幅ケースを 880x800 で
 * 書いている - 同 spec の `NARROW_VIEWPORT` の注記）。ここで固定するのは
 * **その 400px でグリッドが操作できるようになったこと**と、退避パネルの
 * 開閉作法（トグル・選択したら閉じる・Esc・バックドロップ・フォーカス戻し）。
 *
 * ファイル名: `banto-hub-smoke.spec.ts` より辞書順で後（`banto-hub-auth.ts` の
 * 注記参照）。サイドバーの狭幅退避を見る `banto-hub-viewport-offcanvas.spec.ts`
 * と同じ `viewport-` prefix を借り、その後ろ（`viewport-o` < `viewport-s` <
 * `viewport-t`）に並ぶ。ビューポート操作の作法（狭幅の専用ページを
 * `browser.newPage({ viewport })` で作る・`transition` 中の bounding box を
 * 1回だけ見ない）も同 spec に倣う。
 *
 * **「一覧から挿入」トグル（#342 段階C）との Esc 競合は、狭幅では UI から
 * 再現できない**ため固定していない: 狭幅では編集/新規作成フォームが
 * `<Drawer>`/`<Modal>` へ落ち、そのオーバーレイ（z-index 900）がツールバーの
 * 「ツリー」ボタンを覆うので、トグルが ON の状態で退避ツリーを開く操作
 * そのものが存在しない。代わりに**再現できる**同種の競合 - 退避ツリーの上に
 * 開いたモーダルの Esc - をテスト4で固定する（`SplitPane.svelte` の window
 * フォールバックが `role="dialog"` 内の Esc を譲ること）。タグ登録側の
 * 二重の担保（`insertArmed` の window リスナーが `treeOpen` を条件にする）は
 * 経路に依存しないための防御として実装に残してある。
 *
 * 前提データは UI ではなく `page.request` で直接 REST を叩いて作り
 * （`banto-hub-tags-edit-pane.spec.ts` と同じパターン）、後始末は共有ヘルパー
 * `cleanupFixtures` を `beforeAll` の先頭と `afterAll` の両方で呼ぶ
 * （`banto-hub-fixture-cleanup.ts` 冒頭の doc comment）。
 */
import { expect, test, type Locator, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';
import { cleanupFixtures } from './banto-hub-fixture-cleanup';

/** #378 の実測条件そのもの（issue 本文の「400px 幅」）。 */
const NARROW_VIEWPORT = { width: 400, height: 800 };

const CONNECTION_NAME = 'e2e-tree-oc-plc';
const GROUP_A = 'e2e-tree-oc-group-a';
const GROUP_B = 'e2e-tree-oc-group-b';
const TAG_A = 'e2e-tree-oc-tag-a';
const TAG_B = 'e2e-tree-oc-tag-b';
const TAG_B_EXTERNAL = `${CONNECTION_NAME}.${GROUP_B}.${TAG_B}`;

const CLEANUP_TARGET = {
	groupNames: [GROUP_A, GROUP_B],
	connectionNames: [CONNECTION_NAME]
};

test.describe.serial('banto-hub 狭幅でツリーペインを退避する (#378)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	/** タグ登録ページの退避パネル（`SplitPane` の `leftId`）。 */
	let treePane: Locator;
	let treeToggle: Locator;
	let treeSelection: Locator;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: NARROW_VIEWPORT });
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await cleanupFixtures(page.request, authedHeaders, CLEANUP_TARGET);

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

		const groupIds: Record<string, number> = {};
		for (const name of [GROUP_A, GROUP_B]) {
			const groupRes = await page.request.post('/api/collection-groups', {
				headers: authedHeaders,
				data: { name, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
			});
			expect(groupRes.ok()).toBe(true);
			groupIds[name] = ((await groupRes.json()) as { id: number }).id;
		}

		// modbus-tcp 接続配下なので Modbus 参照番号形式のアドレス
		// （`banto-hub-tags-edit-pane.spec.ts` の注記と同じ理由）。
		for (const [name, group, address] of [
			[TAG_A, GROUP_A, '40301'],
			[TAG_B, GROUP_B, '40302']
		] as const) {
			const tagRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: {
					name,
					collectionGroupId: groupIds[group],
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

		treePane = page.locator('#tags-tree-pane');
		treeToggle = page.getByTestId('tag-tree-toggle');
		treeSelection = page.getByTestId('tag-tree-selection');
	});

	test.afterAll(async () => {
		await cleanupFixtures(page.request, authedHeaders, CLEANUP_TARGET);
		await page.close();
	});

	test('1. 400px ではツリーが退避していて、グリッドの行をクリックできる', async () => {
		await page.goto('/tags');
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();

		// 退避しているので支援技術・テストから見ても「閉じている」
		// （`visibility: hidden` + `inert` + `aria-hidden` - `SplitPane.svelte`
		// の CSS コメント参照。`transform` だけだと祖先の `overflow: hidden` で
		// clip されているだけの「見えないが可視」な状態になってしまう）。
		await expect(treePane).toBeHidden();
		await expect(treeToggle).toBeVisible();
		await expect(treeToggle).toHaveAttribute('aria-expanded', 'false');
		await expect(treeSelection).toHaveText('すべて');

		// #378 の本題: 400px でも行がクリックできる（以前はここで
		// actionability チェックがタイムアウトしていた）。`BantoGrid` は行を
		// 仮想化しているので、名前でクリックする前に検索ボックスで絞る。
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_A);
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();

		// 狭幅なので編集フォームは従来どおりオーバーレイの Drawer（#375）。
		const editDrawer = page.getByRole('dialog', { name: `${TAG_A} を編集` });
		await expect(editDrawer).toBeVisible();

		// 未保存が無いので Esc で閉じる（退避ツリーは閉じているため、この Esc は
		// 従来どおり Drawer のものになる - 「閉じているときは他のハンドラに
		// 任せる」）。
		await page.keyboard.press('Escape');
		await expect(editDrawer).toBeHidden();

		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('2. 「ツリー」ボタンで開き、グループを選ぶと閉じてグリッドが絞られる', async () => {
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await expect(treeToggle).toHaveAttribute('aria-expanded', 'true');

		await groupNodeByName(page, GROUP_B).click();

		// 選んだら閉じる（`Sidebar.svelte` のリンククリックと同じ「閉じる契機」）。
		await expect(treePane).toBeHidden();
		await expect(treeToggle).toHaveAttribute('aria-expanded', 'false');
		// 閉じていても何で絞られているかが分かる。
		await expect(treeSelection).toHaveText(GROUP_B);
		// 実際に絞り込まれている（登録先の表示もそのグループになる）。
		await expect(page.getByTestId('tag-registration-target')).toContainText(GROUP_B);
		await expect(page.getByRole('gridcell', { name: TAG_B, exact: true })).toBeVisible();
		await expect(page.getByRole('gridcell', { name: TAG_A, exact: true })).toHaveCount(0);
	});

	test('3. Esc とバックドロップのクリックで閉じ、フォーカスはボタンへ戻る', async () => {
		await treeToggle.click();
		await expect(treePane).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
		// 開く操作をしたボタンへフォーカスを戻す（`TreeContextMenu.svelte` と
		// 同じ `triggerEl` の形）。
		await expect(treeToggle).toBeFocused();

		await treeToggle.click();
		await expect(treePane).toBeVisible();

		// バックドロップは `.split-pane` の中だけを覆う `position: absolute` で、
		// その左端 280px は退避パネル自身が重なっている（z-index はパネルが上）。
		// 既定の中心座標だとパネルに intercept されうるので、パネルより右側の
		// 座標を明示してクリックする（`banto-hub-viewport-offcanvas.spec.ts` の
		// テスト4で実測した罠と同じ）。
		const backdrop = page.getByRole('button', { name: '背景をクリックして接続とグループを閉じる' });
		await expect(backdrop).toBeVisible();
		const box = await backdrop.boundingBox();
		expect(box).not.toBeNull();
		await backdrop.click({ position: { x: (box?.width ?? 0) - 10, y: (box?.height ?? 0) / 2 } });

		await expect(treePane).toBeHidden();
		await expect(treeToggle).toBeFocused();
	});

	test('4. 退避ツリーの上に開いたモーダルの Esc は、モーダルだけを閉じる', async () => {
		await treeToggle.click();
		await expect(treePane).toBeVisible();

		// ツリーペイン内の常設ボタン（T19 S1-a）。退避パネルは閉じないので、
		// モーダルと退避パネルが同時に開いた状態を作れる。
		await treePane.getByRole('button', { name: 'PLC接続を追加' }).click();
		const createModal = page.getByRole('dialog', { name: '新規作成' });
		await expect(createModal).toBeVisible();

		await page.keyboard.press('Escape');

		// 手前（z-index 900）のモーダルが閉じ、退避パネル（610）は開いたまま。
		await expect(createModal).toBeHidden();
		await expect(treePane).toBeVisible();

		// 後片付け: モーダルが無くなった状態の Esc は退避パネルへ届く
		// （window フォールバック - `SplitPane.svelte` の Esc の doc comment）。
		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('5. タグモニタでも 400px でツリーを開いて絞り込める', async () => {
		await page.goto('/monitor');
		await expect(page.getByRole('heading', { level: 2, name: 'タグモニタ' })).toBeVisible();

		const monitorTreePane = page.locator('#monitor-tree-pane');
		const monitorToggle = page.getByTestId('monitor-tree-toggle');
		await expect(monitorTreePane).toBeHidden();
		await expect(monitorToggle).toBeVisible();

		await monitorToggle.click();
		await expect(monitorTreePane).toBeVisible();

		await groupNodeByName(page, GROUP_B).click();

		await expect(monitorTreePane).toBeHidden();
		await expect(page.getByTestId('monitor-tree-selection')).toHaveText(GROUP_B);
		await expect(page.getByRole('cell', { name: TAG_B_EXTERNAL, exact: true })).toBeVisible();
	});
});
