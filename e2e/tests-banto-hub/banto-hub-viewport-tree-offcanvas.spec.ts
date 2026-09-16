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
 * 開いたモーダルの Esc - をテスト4・5で固定する（`SplitPane.svelte` の window
 * フォールバックが、可視な上位層があるあいだ Esc を譲ること。フォーカスが
 * モーダルの外にある場合もテスト5で見る）。タグ登録側の
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

	test('5. モーダルの外にフォーカスがある状態の Esc でも、閉じるのはモーダルだけ（#381 レビュー対応2回目）', async () => {
		// `Drawer.svelte`/`Modal.svelte` はタブ移動を閉じ込めない（同ファイル冒頭
		// doc）ので、モーダルが開いたままフォーカスがその外にある状態がありえる。
		// 退避パネルの window フォールバックが**発生元**だけを見ていると、この
		// 状態の Esc で「モーダルと退避パネルが両方閉じる」ことになる。
		//
		// モーダルは（テスト4 と同じく）ツリーペイン内の常設ボタンから開く -
		// 退避パネルが開いている間はバックドロップがグリッド側を覆うので、
		// 「ツリーもモーダルも開いている」状態を作れる経路はこれになる。
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await treePane.getByRole('button', { name: 'PLC接続を追加' }).click();
		const createModal = page.getByRole('dialog', { name: '新規作成' });
		await expect(createModal).toBeVisible();

		// フォーカスをモーダルの外（body）へ出す。以降の Esc の発生元は
		// モーダルの中ではないので、判定は「可視な上位層があるか」だけが頼り。
		await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
		await page.keyboard.press('Escape');

		await expect(createModal).toBeHidden();
		await expect(treePane).toBeVisible();

		// 後片付け（上位層が無くなったので Esc は退避パネルへ届く）。
		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('6. 狭幅で開いたまま広幅へ広げて狭幅へ戻すと、ツリーは閉じている（#381 レビュー対応A）', async () => {
		await treeToggle.click();
		await expect(treePane).toBeVisible();

		// 広幅では退避そのものが無効になる: トグルも退避パネルの id も消え、
		// ツリーは従来どおり常時表示の左ペインへ戻る（広幅の DOM は無変更）。
		await page.setViewportSize({ width: 1280, height: 800 });
		await expect(treeToggle).toHaveCount(0);
		await expect(treePane).toHaveCount(0);

		// 狭幅へ戻したとき、`leftOpen` が残っていて開いた状態で現れては
		// いけない（`SplitPane` が広幅遷移で閉じへ戻す）。
		await page.setViewportSize(NARROW_VIEWPORT);
		await expect(treeToggle).toBeVisible();
		await expect(treeToggle).toHaveAttribute('aria-expanded', 'false');
		await expect(treePane).toBeHidden();
	});

	test('7. 狭幅でノードを右クリックしてもツリーは閉じず、Esc はメニューだけを閉じる（#381 レビュー対応B/C）', async () => {
		await treeToggle.click();
		await expect(treePane).toBeVisible();

		const node = groupNodeByName(page, GROUP_A);
		await node.click({ button: 'right' });

		const menu = page.getByRole('menu', { name: '作成メニュー' });
		await expect(menu).toBeVisible();
		// 右クリックでも選択は反映されるが、**退避パネルは開いたまま**
		// （閉じるとフォーカスの戻り先が `inert` の中に取り残される）。
		await expect(treePane).toBeVisible();
		await expect(treeSelection).toHaveText(GROUP_A);

		// メニュー（z-index 1000）の Esc はメニューだけを閉じ、退避パネルは
		// 残る。フォーカスは右クリックしたノードへ戻る（`TreeContextMenu` の
		// `triggerEl` が生きている）。
		await page.keyboard.press('Escape');
		await expect(menu).toHaveCount(0);
		await expect(treePane).toBeVisible();
		await expect(node).toBeFocused();

		// 後片付け（メニューが無くなったので Esc は退避パネルへ届く）。
		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('8. サイドバーと退避ツリーが両方開いていても、Esc は1層ずつ畳む（#381 レビュー対応3回目）', async () => {
		await treeToggle.click();
		await expect(treePane).toBeVisible();

		// ☰ はヘッダーにあり、退避ツリーのバックドロップ（`.split-pane` の中
		// だけを覆う）には隠れないので、ツリーを開いたままサイドバーも開ける。
		// サイドバー（z-index 710）はツリー（610）より手前の層。
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		await expect(page.getByRole('button', { name: 'メニューを閉じる', exact: true })).toBeVisible();

		// 1回目の Esc: 手前のサイドバーだけが閉じる（レイアウト側の Esc ハンドラが
		// `preventDefault` してイベントを消費し、`SplitPane` は譲る）。
		await page.keyboard.press('Escape');
		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expect(treePane).toBeVisible();

		// 2回目の Esc: 今度はツリーが閉じる。
		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('9. ツリー・サイドバー・コマンドパレットが重なっても、Esc は手前から1層ずつ畳む（#381 レビュー対応4回目）', async () => {
		// 3層を下から順に開く（退避ツリー 610 → サイドバー 710 →
		// コマンドパレット 1000。`escLayering.ts` の層の表）。パレットは
		// `Ctrl+K` で開く - サイドバーのバックドロップがヘッダーの 🔍 を覆うため、
		// この状態でパレットを開く経路はキーボードになる。
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		await expect(page.getByRole('button', { name: 'メニューを閉じる', exact: true })).toBeVisible();
		await page.keyboard.press('Control+k');

		// コマンドパレットは `role="dialog"`（`CommandPalette.svelte`）なので、
		// 下の2層はどちらもこれを「可視な上位層」として認識して譲る。
		const palette = page.getByRole('dialog', { name: 'コマンドパレット' });
		await expect(palette).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(palette).toHaveCount(0);
		// サイドバーもツリーも道連れにならない。
		await expect(page.getByRole('button', { name: 'メニューを閉じる', exact: true })).toBeVisible();
		await expect(treePane).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expect(treePane).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('10. メニューを開いたまま狭幅へ変わっても、閉じたフォーカスは生きた要素へ戻る（#381 レビュー対応5回目）', async () => {
		// 広幅ではツリーが常時表示なので、そこでノードを右クリックしてから狭幅へ
		// 変える。狭幅になった瞬間に退避パネルは閉じ（`inert`）、メニューが覚えて
		// いる戻り先（ツリーノード）は不活性の中に取り残される。
		await page.setViewportSize({ width: 1280, height: 800 });
		await groupNodeByName(page, GROUP_A).click({ button: 'right' });
		const menu = page.getByRole('menu', { name: '作成メニュー' });
		await expect(menu).toBeVisible();

		await page.setViewportSize(NARROW_VIEWPORT);
		await expect(treePane).toBeHidden();
		await expect(menu).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(menu).toHaveCount(0);

		// フォーカスは `<body>` へ落ちず、開き直せるトグルボタンへ送られる。
		await expect
			.poll(() => page.evaluate(() => document.activeElement?.getAttribute('data-testid') ?? null))
			.toBe('tag-tree-toggle');
	});

	test('11. Drawer の上にコマンドパレットを重ねても、Esc は1層ずつ畳む（#381 レビュー対応5回目）', async () => {
		// 狭幅の編集フォームは `<Drawer>`（z-index 900）。その上に `Ctrl+K` で
		// コマンドパレット（1000）を重ねる。Drawer の window Esc ハンドラは
		// **パレットより先に登録されている**（先に開いたので）ため、
		// `defaultPrevented` だけを見ていると同じ Esc で2層とも閉じてしまう。
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_A);
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();
		const editDrawer = page.getByRole('dialog', { name: `${TAG_A} を編集` });
		await expect(editDrawer).toBeVisible();

		await page.keyboard.press('Control+k');
		const palette = page.getByRole('dialog', { name: 'コマンドパレット' });
		await expect(palette).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(palette).toHaveCount(0);
		await expect(editDrawer).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(editDrawer).toBeHidden();

		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('12. コマンドパレットはフォーカスが外に出ていても Esc で閉じる（#381 レビュー対応6回目）', async () => {
		// `CommandPalette.svelte` はフォーカストラップを持たないので、Shift+Tab で
		// フォーカスがパレットの外へ出る。Esc を検索 input の `onkeydown` だけで
		// 処理していると、この状態ではパレットが閉じず、下の層（Drawer・サイドバー・
		// 退避ツリー）は「可視な上位層がある」と見て全員譲るため **Esc が何も
		// 閉じない**（`escLayering.ts` の層の約束・項目3）。
		await page.keyboard.press('Control+k');
		const palette = page.getByRole('dialog', { name: 'コマンドパレット' });
		await expect(palette).toBeVisible();

		await page.keyboard.press('Shift+Tab');
		await expect
			.poll(() =>
				page.evaluate(() => {
					const active = document.activeElement;
					const panel = document.querySelector('[role="dialog"][aria-label="コマンドパレット"]');
					return !!active && !!panel && !panel.contains(active);
				})
			)
			.toBe(true);

		await page.keyboard.press('Escape');
		await expect(palette).toHaveCount(0);
	});

	test('13. 広幅でツリー内にフォーカスがある状態で狭幅へ変えると、フォーカスはトグルへ逃げる（#381 レビュー対応7回目）', async () => {
		// 広幅ではツリーが常時表示で、ノードは普通にフォーカスできる。狭幅へ
		// 変わるとペインは閉じたまま `inert` + `visibility: hidden` になるので、
		// 逃がさないとフォーカスが `<body>` に落ちてキーボード操作の起点を失う。
		await page.setViewportSize({ width: 1280, height: 800 });
		const node = groupNodeByName(page, GROUP_B);
		await node.focus();
		await expect(node).toBeFocused();

		await page.setViewportSize(NARROW_VIEWPORT);

		await expect(treePane).toBeHidden();
		await expect(treeToggle).toBeFocused();
	});

	test('14. 接続 Drawer が開いている間はグループ Drawer を開かない（未保存入力を捨てない、#381 レビュー対応8回目）', async () => {
		// 同じ層（z-index 900）を2つ開くと、Esc の層判定が互いを「手前の層」と
		// 見なしてどちらも閉じなくなる（`escLayering.ts` の doc）。同時に出さない
		// のは呼び出し側の責務だが、**相手を閉じる**側に倒すと相手の未保存入力を
		// 黙って捨てる（#376 で塞いだ事故と同じ）ので、**こちらを開かせない**。
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await treePane.getByRole('button', { name: 'PLC接続を追加' }).click();

		const createModal = page.getByRole('dialog', { name: '新規作成' });
		await expect(createModal).toBeVisible();
		// 接続ウィザードの手順（グループのウィザードとは2段目の文言が違う）。
		await expect(createModal).toContainText('プロトコルと接続先');

		// 入力途中にする（これが捨てられてはいけない）。
		const nameField = createModal.getByLabel('名前');
		await nameField.fill('e2e-tree-oc-wip');

		// モーダルのオーバーレイがツリーを覆うのでクリックでは届かない -
		// キーボードで到達してボタンを押した場合と同じことを直接の click
		// イベントで再現する（`Drawer`/`Modal` はタブ移動を閉じ込めない）。
		await treePane.getByRole('button', { name: '収集グループを追加' }).dispatchEvent('click');

		// 案内が出るだけで、接続側は開いたまま・入力も残る（グループ側は開かない）。
		await expect(page.getByText('先に開いている PLC 接続の設定を閉じてください')).toBeVisible();
		await expect(createModal).toHaveCount(1);
		await expect(createModal).toContainText('プロトコルと接続先');
		await expect(createModal).not.toContainText('接続先と周期');
		await expect(nameField).toHaveValue('e2e-tree-oc-wip');

		// 1つしか開いていないので Esc で閉じられる（未保存なので確認を挟む -
		// `Modal` は dirty のとき Esc では閉じないので `×` から閉じる）。
		page.once('dialog', (dialog) => void dialog.accept());
		await createModal.getByRole('button', { name: '閉じる' }).click();
		await expect(createModal).toHaveCount(0);

		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('15. Modal の中で Tab / Shift+Tab を押してもフォーカスはパネル内に留まる（#381 レビュー対応8回目）', async () => {
		// フォーカストラップ（`focusTrap.ts`）。これが無いと、オーバーレイの裏に
		// ある起動ボタンへ Tab で到達して別の層を開けてしまう（このレビュー往復で
		// 連鎖した症状の入口）。
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await treePane.getByRole('button', { name: 'PLC接続を追加' }).click();
		const createModal = page.getByRole('dialog', { name: '新規作成' });
		await expect(createModal).toBeVisible();

		const focusInsidePanel = () =>
			page.evaluate(() => {
				const panel = document.querySelector('[role="dialog"][aria-modal="true"]');
				const active = document.activeElement;
				return !!panel && !!active && panel.contains(active);
			});

		// パネル内のフォーカス可能要素の数より多く押しても外へ出ない。
		for (let i = 0; i < 12; i += 1) {
			await page.keyboard.press('Tab');
			expect(await focusInsidePanel()).toBe(true);
		}
		for (let i = 0; i < 12; i += 1) {
			await page.keyboard.press('Shift+Tab');
			expect(await focusInsidePanel()).toBe(true);
		}

		await page.keyboard.press('Escape');
		await expect(createModal).toHaveCount(0);
		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('16. サイドバーが開いているときにツリーを開くと、サイドバーが先に畳まれる（#381 レビュー対応8回目）', async () => {
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		await expect(page.getByRole('button', { name: 'メニューを閉じる', exact: true })).toBeVisible();

		// サイドバーのバックドロップが本文を覆うのでクリックでは届かない -
		// Tab でヘッダー経由トグルへ到達して押した場合と同じことを直接の click
		// イベントで再現する（サイドバーはモーダルではないので到達できる）。
		await treeToggle.dispatchEvent('click');

		// 上位のサイドバーは閉じ、ツリーだけが開いている（重なりが逆順にならない）。
		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expect(treePane).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('17. タグモニタでも 400px でツリーを開いて絞り込める', async () => {
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
