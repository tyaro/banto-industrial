/**
 * #1253（docs/hub/banto-hub-ui-improvement-proposal.md §ドック型の配置）: タグ登録（`/tags`）の
 * 枠を `@banto/dock-svelte` の `ThreePaneDock` へ載せ替えたことの実 DOM 受け入れテスト。
 *
 * 1. 配置（ツリー・属性の表示・非表示）は利用者ごとに保存され（`hub.dock.v2.tags`）、URL には
 *    載らず、「表示」メニューの「レイアウトを既定に戻す」で戻せる。
 * 2. 右（属性 = 編集のフォーム）は据え置き（`persistent`）: 隠す・出す・既定に戻す・狭い表示を
 *    行き来しても、未保存の入力が消えず、広い表示では同じ入力欄（DOM）のまま。
 * 3. 狭い幅（900px 以下）での「既定に戻す」も保存される（タグモニタの 6 と同じ型）。
 * 4. 広い幅から狭い幅へ切り替わる途中の一時の配置は保存しない（タグモニタの 7 と同じ型）。
 * 5. #1159: 1366×768・125%（CSS では 1093×614）で右の属性を含めて「保存」まで届き、狭い幅では
 *    編集がモーダルで届く。畳んだ右も帯のタブから開ける。
 * 6. PR #1269 のレビュー: 広い幅で属性を隠してから狭い幅で編集を始め、広い幅へ戻すと、属性が
 *    出て編集のフォームが見え、入力した値が残っている（値はページの状態。DOM は入れ替わる）。
 * 7. PR #1269 のレビュー: 狭い幅で「表示」メニューから属性をキーボードで開くと、フォーカスが
 *    引き出した pane の中へ入り、Esc で閉じてフォーカスが帯のタブへ戻る。
 * 8. PR #1269 のレビュー（P2）: 広い幅で属性を帯に折りたたんでから一覧のタグを選ぶと、属性が
 *    展開されて編集のフォームが見える（元の作りの `editPaneCollapsed = false` と同じ）。
 * 9. PR #1269 のレビュー（P2）: 属性を自動で隠す帯へ入れてから一覧のタグを選ぶと、引き出す
 *    （一覧を押すと帯へ戻る）のではなく元の場所へ戻り、一覧を押しても編集のフォームが見えたまま。
 * 10〜12. 2026-11-21 オーナー決定: 低い画面（高さ 900px 未満）では、部品が作った既定の配置
 *    （保存値が無い・「既定に戻す」）でログを下端の自動で隠す帯で始める。1280×720 では帯で、
 *    タグ一覧の行が見え、帯のタブから引き出せる。帯で始めた既定は保存しない。1920×1080 では
 *    下に開く。低い画面の「既定に戻す」も帯。保存値はサーバーの値を空にして消す。
 * 13〜14. PR #1270 のレビュー（P2）: 狭い幅で始めた・狭い幅で既定に戻したときも、低い画面の
 *    補正は部品が控えた広い配置そのものに当たる。狭い幅（880×800）で既定から始めて幅だけ
 *    広げると、ログは帯で、配置を書かない（サーバー・localStorage とも）。狭い幅の「既定に戻す」
 *    は、広い幅へ戻る前に帯の配置を保存する。
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

/** 高い画面（ログを下に開いて始める）。 */
const TALL = { width: 1920, height: 1080 };

const DOCK_SETTING = '/api/ui-settings/hub.dock.v2.tags';

/** 狭い幅・低い画面（ログを帯で始める高さ）と、そこから幅だけ広げた大きさ。 */
const NARROW_LOW = { width: 880, height: 800 };
const WIDE_LOW = { width: 1280, height: 800 };

/** 保存値（JSON）のログが下端の帯（`autoHidden`）にあるか。 */
function logIsBanded(raw: string | null): boolean {
	if (raw === null) return false;
	const value = JSON.parse(raw) as { layout?: { autoHidden?: { node: unknown }[] } };
	return (value.layout?.autoHidden ?? []).some((entry) =>
		JSON.stringify(entry.node).includes('"id":"log"')
	);
}

test.describe.serial('banto-hub タグ登録のドックの枠 (#1253)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;

	const tree = () => page.getByTestId('tag-tree-pane');
	const treeToggle = () => page.getByTestId('tag-tree-toggle');
	const attributeToggle = () => page.getByTestId('tag-attribute-toggle');
	const editPane = () => page.getByRole('complementary', { name: `${TAG_NAME} を編集` });
	const editDrawer = () => page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
	const logPanel = () => page.getByTestId('log-panel');
	const logBandTab = () => page.locator('button[data-dock-band-tab="log"]');
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

	/** 保存した配置を消す（サーバーの値を空に・localStorage の写しを消す）= 部品の既定から始める。 */
	async function clearSavedLayout(): Promise<void> {
		const res = await page.request.put(DOCK_SETTING, {
			headers: authedHeaders,
			data: { value: '' }
		});
		expect(res.ok(), await res.text()).toBe(true);
		await page.evaluate(() => {
			for (const key of Object.keys(localStorage)) {
				if (key.startsWith('banto.dock.v2.tags.')) localStorage.removeItem(key);
			}
		});
	}

	/** サーバーに保存した配置（空なら `null`）。 */
	async function savedLayout(): Promise<string | null> {
		const res = await page.request.get(DOCK_SETTING, { headers: authedHeaders });
		expect(res.ok()).toBe(true);
		const body = (await res.json()) as { value: string | null };
		return body.value === '' ? null : body.value;
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
					.filter((k) => k.startsWith('banto.dock.v2.tags.'))
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
	test('6. 狭い幅で始めた編集は、隠していた属性を広い幅へ戻したときに出す（PR #1269 のレビュー）', async () => {
		await openTags();
		const hidden = dockSetting('PUT');
		await attributeToggle().click();
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'false');
		expect((await hidden).ok()).toBe(true);

		try {
			await page.setViewportSize(NARROW);
			await expect(treeToggle()).toHaveCount(0);
			await openEditor();
			await expect(editDrawer()).toBeVisible();
			await editDrawer().getByLabel('単位').fill('kPa');
		} finally {
			await page.setViewportSize(WIDE);
		}
		await expect(editDrawer()).toHaveCount(0);
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'true');
		await expect(editPane()).toBeVisible();
		await expect(editPane().getByLabel('単位')).toHaveValue('kPa');

		// 後片付け: 入力を捨てて閉じ、既定の配置へ戻す。
		page.once('dialog', (dialog) => void dialog.accept());
		await editPane().getByRole('button', { name: '閉じる' }).click();
		await expect(editPane()).toHaveCount(0);
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
		await resetLayout();
		await expect(treeToggle()).toHaveAttribute('aria-pressed', 'true');
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'true');
	});

	test('7. 狭い幅の「表示」→「属性」をキーボードで開くと、Esc で閉じて帯のタブへ戻る（PR #1269 のレビュー）', async () => {
		await page.setViewportSize(NARROW);
		try {
			await openTags();
			await expect(treeToggle()).toHaveCount(0);
			const flyout = page.locator('[data-dock-flyout="right"]');
			const bandTab = page.locator('button[data-dock-band-tab="right"]');
			await expect(bandTab).toBeVisible();

			const viewMenu = page.getByTestId('tag-view-menu');
			await viewMenu.focus();
			await page.keyboard.press('Enter');
			const item = page.getByRole('menuitem', { name: '属性', exact: true });
			await expect(item).toBeVisible();
			await item.focus();
			await page.keyboard.press('Enter');

			await expect(page.getByTestId('tag-attribute-empty')).toBeVisible();
			// フォーカスは引き出した pane の中（「表示」に戻っていない）。
			await expect
				.poll(() =>
					page.evaluate(() => !!document.activeElement?.closest('[data-dock-flyout="right"]'))
				)
				.toBe(true);

			await page.keyboard.press('Escape');
			await expect(flyout).toBeHidden();
			await expect(bandTab).toBeFocused();
		} finally {
			await page.setViewportSize(WIDE);
		}
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'true');
	});

	/** 編集を閉じ、検索を空にして既定の配置へ戻す（8・9 の後片付け）。 */
	async function closeEditorAndReset(): Promise<void> {
		await editPane().getByRole('button', { name: '閉じる' }).click();
		await expect(editPane()).toHaveCount(0);
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
		await resetLayout();
		await expect(page.getByRole('button', { name: '属性を帯から展開' })).toHaveCount(0);
		await expect(page.locator('button[data-dock-band-tab="right"]')).toHaveCount(0);
		await expect(attributeToggle()).toHaveAttribute('aria-pressed', 'true');
	}

	test('8. 帯に折りたたんだ属性は、一覧のタグを選ぶと展開して編集のフォームを出す（PR #1269 のレビュー）', async () => {
		await openTags();
		await page.getByRole('button', { name: '属性を帯に折りたたむ' }).click();
		const expand = page.getByRole('button', { name: '属性を帯から展開' });
		await expect(expand).toBeVisible();
		await expect(page.getByTestId('tag-attribute-empty')).toBeHidden();

		await openEditor();
		await expect(expand).toHaveCount(0);
		await expect(editPane()).toBeVisible();
		await expect(editPane().getByLabel('単位')).toBeVisible();

		await closeEditorAndReset();
	});

	test('9. 自動で隠した属性は、一覧のタグを選ぶと元の場所へ戻り、一覧を押しても見えたまま（PR #1269 のレビュー）', async () => {
		await openTags();
		await page.getByRole('button', { name: '属性を自動で隠す（ピンを外す）' }).click();
		const bandTab = page.locator('button[data-dock-band-tab="right"]');
		await expect(bandTab).toBeVisible();
		await expect(page.getByTestId('tag-attribute-empty')).toBeHidden();

		await openEditor();
		await expect(bandTab).toHaveCount(0);
		await expect(page.locator('[data-dock-flyout="right"]')).toHaveCount(0);
		await expect(editPane()).toBeVisible();
		// 引き出しただけなら一覧（外）の押下で帯へ戻る。元の場所へ戻したので見えたまま。
		await page.getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		await expect(editPane().getByLabel('単位')).toBeVisible();
		await expect(bandTab).toHaveCount(0);

		await closeEditorAndReset();
	});

	test('10. 低い画面（1280×720）の既定ではログを帯で始め、一覧の行が見え、帯のタブから引き出せる（2026-11-21 オーナー決定）', async () => {
		await clearSavedLayout();
		const puts: string[] = [];
		const onRequest = (req: Request) => {
			if (req.url().includes(DOCK_SETTING) && req.method() === 'PUT')
				puts.push(req.postData() ?? '');
		};
		page.on('request', onRequest);
		try {
			await openTags();
			await expect(logBandTab()).toBeVisible();
			await expect(logPanel()).toBeHidden();
			await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_NAME);
			await expect(page.getByRole('gridcell', { name: TAG_NAME, exact: true })).toBeInViewport();

			// 帯のタブから引き出せる。外の Esc で引っ込む。
			await logBandTab().click();
			await expect(logPanel()).toBeVisible();
			await page.keyboard.press('Escape');
			await expect(logPanel()).toBeHidden();
			await expect(logBandTab()).toBeVisible();

			// 帯で始めた既定も、引き出し・引っ込めも、利用者の配置の変更として書かない。
			await page.waitForTimeout(800);
			expect(puts).toEqual([]);
			expect(await savedLayout()).toBeNull();
		} finally {
			page.off('request', onRequest);
		}
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('11. 高い画面（1920×1080）の既定ではログを下に開く', async () => {
		await page.setViewportSize(TALL);
		try {
			await clearSavedLayout();
			await openTags();
			await expect(logPanel()).toBeVisible();
			await expect(logBandTab()).toHaveCount(0);
		} finally {
			await page.setViewportSize(WIDE);
		}
		// 高さが変わっても開いた配置は畳み直さない（利用者の配置と同じ扱い）。
		await expect(logPanel()).toBeVisible();
		await clearSavedLayout();
	});

	test('12. 低い画面の「レイアウトを既定に戻す」もログを帯にし、それを保存する。最後は既定の配置', async () => {
		await clearSavedLayout();
		await openTags();
		await expect(logBandTab()).toBeVisible();

		// ツリーを隠し、ログを下にドッキングし直す（帯のタブで引き出し、ピンで戻す）。
		const hidden = dockSetting('PUT');
		await treeToggle().click();
		await expect(tree()).toBeHidden();
		expect((await hidden).ok()).toBe(true);
		await logBandTab().click();
		await expect(logPanel()).toBeVisible();
		await page.getByRole('button', { name: 'ログを元の場所に固定（ピンを留める）' }).click();
		await expect(logBandTab()).toHaveCount(0);
		await expect(logPanel()).toBeVisible();
		await expect
			.poll(async () => (await savedLayout())?.includes('"autoHidden"') ?? false)
			.toBe(false);

		await resetLayout();
		await expect(tree()).toBeVisible();
		await expect(logBandTab()).toBeVisible();
		await expect(logPanel()).toBeHidden();
		await expect
			.poll(async () => {
				const value = await savedLayout();
				return value !== null && value.includes('"autoHidden"') && value.includes('"log"');
			})
			.toBe(true);

		// 開き直しても帯（保存した既定に従う）。
		await openTags();
		await expect(logBandTab()).toBeVisible();
		await expect(tree()).toBeVisible();

		// 既定の配置（保存値なし）で終える。
		await clearSavedLayout();
	});
	const localCopies = () =>
		page.evaluate(() =>
			Object.keys(localStorage)
				.filter((k) => k.startsWith('banto.dock.v2.tags.'))
				.sort()
				.map((k) => [k, localStorage.getItem(k)])
		);

	test('13. 狭い幅（低い画面）で既定から始めて幅だけ広げると、ログは帯で、配置を書かない（PR #1270 のレビュー）', async () => {
		await clearSavedLayout();
		const puts: string[] = [];
		const onRequest = (req: Request) => {
			if (req.url().includes(DOCK_SETTING) && req.method() === 'PUT')
				puts.push(req.postData() ?? '');
		};
		await page.setViewportSize(NARROW_LOW);
		page.on('request', onRequest);
		try {
			await openTags();
			await expect(treeToggle()).toHaveCount(0);
			await page.waitForTimeout(800);
			await page.setViewportSize(WIDE_LOW);
			await expect(treeToggle()).toHaveAttribute('aria-pressed', 'true');
			await expect(logBandTab()).toBeVisible();
			await expect(logPanel()).toBeHidden();
			await page.waitForTimeout(800);
			expect(puts).toEqual([]);
			expect(await localCopies()).toEqual([]);
			expect(await savedLayout()).toBeNull();
		} finally {
			page.off('request', onRequest);
			await page.setViewportSize(WIDE);
		}
		await clearSavedLayout();
	});

	test('14. 狭い幅（低い画面）の「既定に戻す」は、広い幅へ戻る前に帯の配置を保存する（PR #1270 のレビュー）', async () => {
		// 広い幅でログを下にドッキングし直した配置を保存しておく（既定と違う値）。
		await clearSavedLayout();
		await page.setViewportSize(WIDE_LOW);
		try {
			await openTags();
			await expect(logBandTab()).toBeVisible();
			await logBandTab().click();
			await expect(logPanel()).toBeVisible();
			const pinned = dockSetting('PUT');
			await page.getByRole('button', { name: 'ログを元の場所に固定（ピンを留める）' }).click();
			expect((await pinned).ok()).toBe(true);
			await expect.poll(async () => logIsBanded(await savedLayout())).toBe(false);

			await page.setViewportSize(NARROW_LOW);
			await openTags();
			await expect(treeToggle()).toHaveCount(0);
			await page.waitForTimeout(800);

			const puts: string[] = [];
			const onRequest = (req: Request) => {
				if (req.url().includes(DOCK_SETTING) && req.method() === 'PUT')
					puts.push(req.postData() ?? '');
			};
			page.on('request', onRequest);
			try {
				await resetLayout();
				await expect.poll(() => puts.length).toBeGreaterThan(0);
				await page.waitForTimeout(800);
			} finally {
				page.off('request', onRequest);
			}
			// 広い幅へ戻る前に、送った値・localStorage の写し・サーバーの値がどれも帯。
			for (const body of puts) {
				expect(logIsBanded((JSON.parse(body) as { value: string }).value)).toBe(true);
			}
			const copies = await localCopies();
			expect(copies).toHaveLength(1);
			expect(logIsBanded(copies[0][1])).toBe(true);
			expect(logIsBanded(await savedLayout())).toBe(true);
		} finally {
			await page.setViewportSize(WIDE);
		}
		await clearSavedLayout();
	});

	test('15. サーバーの保存値が外形だけ正しく中身が使えない（部品が既定へ戻す）ときも、低い画面ではログを帯にし、配置を書かない（PR #1270 再レビュー P2）', async () => {
		const broken = JSON.stringify({ schema: 1, panels: [], layout: {} });
		for (const target of [
			{ path: '/tags', setting: DOCK_SETTING, localPrefix: 'banto.dock.v2.tags.' },
			{
				path: '/monitor',
				setting: '/api/ui-settings/hub.dock.v2.monitor',
				localPrefix: 'banto.dock.v2.monitor.'
			}
		]) {
			const put = await page.request.put(target.setting, {
				headers: authedHeaders,
				data: { value: broken }
			});
			expect(put.ok(), await put.text()).toBe(true);
			await page.evaluate((prefix) => {
				for (const key of Object.keys(localStorage)) {
					if (key.startsWith(prefix)) localStorage.removeItem(key);
				}
			}, target.localPrefix);
			const puts: string[] = [];
			const onRequest = (req: Request) => {
				if (req.url().includes(target.setting) && req.method() === 'PUT')
					puts.push(req.postData() ?? '');
			};
			page.on('request', onRequest);
			try {
				const loaded = page.waitForResponse(
					(r) => r.url().includes(target.setting) && r.request().method() === 'GET'
				);
				await page.goto(target.path);
				await loaded;
				await page.waitForTimeout(800);
				await expect(logBandTab()).toBeVisible();
				await expect(logPanel()).toBeHidden();
				// 意図しない PUT が無い（送られたなら帯の値）。サーバーの値は壊れたまま残る。
				for (const body of puts) {
					expect(logIsBanded((JSON.parse(body) as { value: string }).value)).toBe(true);
				}
				expect(puts).toEqual([]);
			} finally {
				page.off('request', onRequest);
			}
			// 既定の配置（保存値なし）で終える。
			const clear = await page.request.put(target.setting, {
				headers: authedHeaders,
				data: { value: '' }
			});
			expect(clear.ok(), await clear.text()).toBe(true);
			await page.evaluate((prefix) => {
				for (const key of Object.keys(localStorage)) {
					if (key.startsWith(prefix)) localStorage.removeItem(key);
				}
			}, target.localPrefix);
		}
	});
});
