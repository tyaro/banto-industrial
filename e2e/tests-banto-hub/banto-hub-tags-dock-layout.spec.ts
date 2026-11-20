/**
 * #1253（docs/hub/banto-hub-ui-improvement-proposal.md §ドック型の配置）: タグ登録（`/tags`）の
 * 枠を `@banto/dock-svelte` の `ThreePaneDock` へ載せ替えたことの実 DOM 受け入れテスト。
 *
 * 1. 配置（ツリー・属性の表示・非表示）は利用者ごとに保存され（`hub.dock.tags`）、URL には
 *    載らず、「表示」メニューの「レイアウトを既定に戻す」で戻せる。
 * 2. 右（属性 = 編集のフォーム）は据え置き（`persistent`）: 隠す・出す・既定に戻す・狭い表示を
 *    行き来しても、未保存の入力が消えず、広い表示では同じ入力欄（DOM）のまま。
 * 3. 狭い幅（900px 以下）での「既定に戻す」も保存される（タグモニタの 6 と同じ型）。
 * 4. 広い幅から狭い幅へ切り替わる途中の一時の配置は保存しない（タグモニタの 7 と同じ型）。
 * 5. #1159: 1366×768・125%（CSS では 1093×614）で右の属性を含めて「保存」まで届き、狭い幅では
 *    編集がモーダルで届く。畳んだ右も帯のタブから開ける。
 *
 * どのテストも既定の配置で終える（後の spec に残さない）。ファイル名は
 * `banto-hub-smoke.spec.ts` より辞書順で後（`banto-hub-auth.ts` の注記）。前提データは REST で
 * 作り、名前は実行ごとに一意にする（§3 の `describe.serial` の注意）。
 */
import { expect, test, type Page, type Request } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';
import { cleanupFixtures } from './banto-hub-fixture-cleanup';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-dock-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-dock-group-${RUN_ID}`;
const TAG_NAME = `e2e-dock-tag-${RUN_ID}`;
const CLEANUP_TARGET = { groupNames: [GROUP_NAME], connectionNames: [CONNECTION_NAME] };

const WIDE = { width: 1280, height: 720 };
/** 狭い表示（ビューポートが 900px 以下）。グリッドを操作できる幅（tags-edit-pane と同じ）。 */
const NARROW = { width: 880, height: 800 };
/** #1159: 1366×768 を 125% で表示したときの CSS の大きさ。 */
const SCALED_125 = { width: 1093, height: 614 };

const DOCK_SETTING = '/api/ui-settings/hub.dock.tags';

test.describe.serial('banto-hub タグ登録のドックの枠 (#1253)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;

	const tree = () => page.getByTestId('tag-tree-pane');
	const treeToggle = () => page.getByTestId('tag-tree-toggle');
	const attributeToggle = () => page.getByTestId('tag-attribute-toggle');
	const editPane = () => page.getByRole('complementary', { name: `${TAG_NAME} を編集` });
	const editDrawer = () => page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
	const dockSetting = (method: string) =>
		page.waitForResponse((r) => r.url().includes(DOCK_SETTING) && r.request().method() === method);

	async function openTags(): Promise<void> {
		const loaded = dockSetting('GET');
		await page.goto('/tags');
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		await loaded;
	}

	async function resetLayout(): Promise<void> {
		await page.getByTestId('tag-view-menu').click();
		await page.getByRole('menuitem', { name: 'レイアウトを既定に戻す' }).click();
	}

	async function openEditor(): Promise<void> {
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_NAME);
		await page.getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
	}

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE });
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
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
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
		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: TAG_NAME,
				collectionGroupId: group.id,
				address: '40501',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await cleanupFixtures(page.request, authedHeaders, CLEANUP_TARGET);
		await page.close();
	});

	test('1. ツリーと属性を隠した配置は利用者ごとに保存され、「表示」メニューで既定に戻せる', async () => {
		await openTags();
		await expect(tree()).toBeVisible();
		await expect(treeToggle()).toHaveAttribute('aria-pressed', 'true');
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'true');
		// 編集していないときの右は案内だけ。
		await expect(page.getByTestId('tag-attribute-empty')).toBeVisible();

		const treeHidden = dockSetting('PUT');
		await treeToggle().click();
		await expect(tree()).toBeHidden();
		expect((await treeHidden).ok()).toBe(true);
		const attributeHidden = dockSetting('PUT');
		await attributeToggle().click();
		await expect(page.getByTestId('tag-attribute-empty')).toBeHidden();
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'false');
		expect((await attributeHidden).ok()).toBe(true);

		// 再読み込みしても隠したまま（URL には載せない）。
		await openTags();
		await expect(tree()).toBeHidden();
		await expect(page.getByTestId('tag-attribute-empty')).toBeHidden();
		await expect(page).toHaveURL(/\/tags$/);

		const reset = dockSetting('PUT');
		await resetLayout();
		await expect(tree()).toBeVisible();
		await expect(page.getByTestId('tag-attribute-empty')).toBeVisible();
		expect((await reset).ok()).toBe(true);
		await openTags();
		await expect(tree()).toBeVisible();
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'true');
	});

	test('2. 属性の未保存の入力は、隠す・出す・既定に戻す・狭い表示の行き来で消えない', async () => {
		await openTags();
		await openEditor();
		await expect(editPane()).toBeVisible();
		const unit = editPane().getByLabel('単位');
		await unit.fill('℃');
		// 入力欄の DOM に印を付け、作り直されていないことを見る（据え置き）。
		await unit.evaluate((el) => ((el as unknown as { __dockMark: number }).__dockMark = 1253));
		const sameInput = () =>
			unit.evaluate((el) => (el as unknown as { __dockMark?: number }).__dockMark === 1253);

		// 右を隠しても編集は閉じない（中身は残る）。出し直すと同じ入力欄・同じ値。
		await attributeToggle().click();
		await expect(editPane()).toBeHidden();
		await attributeToggle().click();
		await expect(editPane()).toBeVisible();
		await expect(unit).toHaveValue('℃');
		expect(await sameInput()).toBe(true);

		// 右を隠したまま別の配置へ: 既定に戻すと右が戻り、入力はそのまま。
		await attributeToggle().click();
		await expect(editPane()).toBeHidden();
		await resetLayout();
		await expect(editPane()).toBeVisible();
		await expect(unit).toHaveValue('℃');
		expect(await sameInput()).toBe(true);

		// 狭い表示では編集はモーダル。同じ値で出て、広い表示へ戻すと右に戻る。
		await page.setViewportSize(NARROW);
		try {
			await expect(editDrawer()).toBeVisible();
			await expect(editDrawer().getByLabel('単位')).toHaveValue('℃');
		} finally {
			await page.setViewportSize(WIDE);
		}
		await expect(editDrawer()).toHaveCount(0);
		await expect(editPane()).toBeVisible();
		await expect(editPane().getByLabel('単位')).toHaveValue('℃');

		// 後片付け: 入力を捨てて閉じる。
		page.once('dialog', (dialog) => void dialog.accept());
		await editPane().getByRole('button', { name: '閉じる' }).click();
		await expect(editPane()).toHaveCount(0);
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('3. 狭い幅での「既定に戻す」も保存される（タグモニタの 6 と同じ型）', async () => {
		await openTags();
		const hidden = dockSetting('PUT');
		await treeToggle().click();
		await expect(tree()).toBeHidden();
		expect((await hidden).ok()).toBe(true);

		try {
			// 狭い幅で開き直し、サーバーの保存値を読み終えてから戻す。
			await page.setViewportSize(NARROW);
			await openTags();
			await expect(treeToggle()).toHaveCount(0);

			const reset = dockSetting('PUT');
			await resetLayout();
			expect((await reset).ok()).toBe(true);
		} finally {
			await page.setViewportSize(WIDE);
		}

		// 広い幅で開き直すと既定の配置（ツリーが出ている）。
		await openTags();
		await expect(tree()).toBeVisible();
		await expect(treeToggle()).toHaveAttribute('aria-pressed', 'true');
	});

	test('4. 広い幅から狭い幅へ切り替わる途中の一時の配置は保存しない（タグモニタの 7 と同じ型）', async () => {
		const puts: string[] = [];
		const onRequest = (req: Request) => {
			if (req.url().includes(DOCK_SETTING) && req.method() === 'PUT')
				puts.push(req.postData() ?? '');
		};
		const localCopies = () =>
			page.evaluate(() =>
				Object.keys(localStorage)
					.filter((k) => k.startsWith('banto.dock.tags.'))
					.sort()
					.map((k) => [k, localStorage.getItem(k)])
			);

		await openTags();
		await expect(tree()).toBeVisible();
		const before = await localCopies();

		page.on('request', onRequest);
		try {
			// ツリーの中にフォーカスを置いたまま狭い幅へ（部品はフォーカスを直してから確定する）。
			await groupNodeByName(page, GROUP_NAME).focus();
			await page.setViewportSize(NARROW);
			await expect(treeToggle()).toHaveCount(0);
			await page.waitForTimeout(800);
			expect(puts).toEqual([]);
			expect(await localCopies()).toEqual(before);
		} finally {
			page.off('request', onRequest);
			await page.setViewportSize(WIDE);
		}

		await expect(treeToggle()).toHaveAttribute('aria-pressed', 'true');
		await openTags();
		await expect(tree()).toBeVisible();
	});

	test('5. 1366×768・125% では属性を含めて「保存」まで届き、狭い幅でも編集と属性へ届く（#1159）', async () => {
		await page.setViewportSize(SCALED_125);
		try {
			await openTags();
			await openEditor();
			await expect(editPane()).toBeVisible();
			const save = editPane().getByRole('button', { name: '保存', exact: true });
			await save.scrollIntoViewIfNeeded();
			await expect(save).toBeInViewport();
			// 中央のグリッドもまだ操作できる幅が残っている。
			await expect(page.getByRole('gridcell', { name: TAG_NAME, exact: true })).toBeInViewport();
			await editPane().getByRole('button', { name: '閉じる' }).click();
			await expect(editPane()).toHaveCount(0);

			// 狭い幅: 右は端の帯に畳まれる（保存した配置には書き込まない）。帯のタブで開け、
			// 編集はモーダルで「保存」まで届く。
			await page.setViewportSize(NARROW);
			const attributeTab = page.locator('button[data-dock-band-tab="right"]');
			await expect(attributeTab).toBeVisible();
			await attributeTab.click();
			await expect(page.getByTestId('tag-attribute-empty')).toBeVisible();
			await page.keyboard.press('Escape');
			await expect(page.getByTestId('tag-attribute-empty')).toBeHidden();

			await page.getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
			await expect(editDrawer()).toBeVisible();
			const drawerSave = editDrawer().getByRole('button', { name: '保存', exact: true });
			await drawerSave.scrollIntoViewIfNeeded();
			await expect(drawerSave).toBeInViewport();
			await page.keyboard.press('Escape');
			await expect(editDrawer()).toHaveCount(0);
		} finally {
			await page.setViewportSize(WIDE);
		}
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
		// 既定の配置のまま終える。
		await expect(treeToggle()).toHaveAttribute('aria-pressed', 'true');
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'true');
	});
});
