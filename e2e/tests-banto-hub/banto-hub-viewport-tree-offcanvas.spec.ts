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

		await page.evaluate(() => {
			(window as unknown as { __trace: string[] }).__trace = [];
			document.addEventListener(
				'focusin',
				(e) => {
					const t = e.target as HTMLElement;
					(window as unknown as { __trace: string[] }).__trace.push(
						'in:' + t.tagName + ':' + (t.textContent ?? '').trim().slice(0, 10)
					);
				},
				true
			);
		});
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

	test('12. コマンドパレットもフォーカスを閉じ込め、Esc で閉じる（#381 レビュー対応9回目）', async () => {
		// `aria-modal="true"` を名乗る層は必ずトラップを持つ（層の約束・項目4）。
		// window レベルの Esc 処理（項目3、レビュー6回目で追加）は、フォーカスが
		// 何らかの理由で外に出た場合の保険として残してある。
		await page.keyboard.press('Control+k');
		const palette = page.getByRole('dialog', { name: 'コマンドパレット' });
		await expect(palette).toBeVisible();

		const focusInsidePalette = () =>
			page.evaluate(() => {
				const panel = document.querySelector('[role="dialog"][aria-label="コマンドパレット"]');
				const active = document.activeElement;
				return !!panel && !!active && panel.contains(active);
			});

		await page.keyboard.press('Shift+Tab');
		expect(await focusInsidePalette()).toBe(true);
		await page.keyboard.press('Tab');
		expect(await focusInsidePalette()).toBe(true);

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

	test('17. パレットを閉じるとフォーカスは開いた元へ戻る（#381 レビュー対応9回目）', async () => {
		// `Ctrl+K` は Drawer の中からでも効く。閉じたときにフォーカスを戻さないと
		// `<body>` に落ち、そこからの Tab は**どのパネルの keydown も通らない**ので
		// Drawer のトラップをすり抜ける（層の約束・項目5）。
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_A);
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();
		const editDrawer = page.getByRole('dialog', { name: `${TAG_A} を編集` });
		await expect(editDrawer).toBeVisible();

		const nameField = editDrawer.getByLabel('名前');
		await nameField.focus();
		await expect(nameField).toBeFocused();

		await page.keyboard.press('Control+k');
		await expect(page.getByRole('dialog', { name: 'コマンドパレット' })).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(page.getByRole('dialog', { name: 'コマンドパレット' })).toHaveCount(0);

		// 開いた元（Drawer 内の入力欄）へ戻り、続く Tab も Drawer 内で循環する。
		await expect(nameField).toBeFocused();
		await page.keyboard.press('Tab');
		expect(
			await page.evaluate(() => {
				const panel = document.querySelector('[role="dialog"][aria-modal="true"]');
				const active = document.activeElement;
				return !!panel && !!active && panel.contains(active);
			})
		).toBe(true);

		await page.keyboard.press('Escape');
		await expect(editDrawer).toHaveCount(0);
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('18. Drawer を閉じるとフォーカスは開いた元（グリッドの行）へ戻る（#381 レビュー対応10回目）', async () => {
		// 層の約束・項目5（`escLayering.ts`）。`<body>` に落とすと、そこからの Tab は
		// どのパネルの keydown も通らないので残りの層のトラップをすり抜ける。
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_A);
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();
		const editDrawer = page.getByRole('dialog', { name: `${TAG_A} を編集` });
		await expect(editDrawer).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(editDrawer).toHaveCount(0);

		// 行クリックでフォーカスを受けた要素（グリッド内）へ戻っている
		// （`<body>` ではない）。
		expect(
			await page.evaluate(() => {
				const grid = document.querySelector('[role="grid"]');
				const active = document.activeElement;
				return !!grid && !!active && grid.contains(active);
			})
		).toBe(true);

		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('19. 広幅: 右クリックメニューから開いた Drawer を閉じると、フォーカスはノードへ戻る（#381 レビュー対応11回目）', async () => {
		// メニューは項目を選んだ直後にアンマウントされる（`TreeContextMenu.activate`）
		// ので、Drawer が覚えている「開いた元」＝メニュー項目は閉じるころには DOM に
		// 居ない。呼び出し側が渡す `focusFallback`（右クリックしたノード）で拾う。
		await page.setViewportSize({ width: 1280, height: 800 });
		const node = groupNodeByName(page, GROUP_A);
		await node.click({ button: 'right' });
		await page.getByRole('menuitem', { name: '収集グループを再設定', exact: true }).click();

		const groupDrawer = page.getByRole('dialog', { name: `${GROUP_A} を編集` });
		await expect(groupDrawer).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(groupDrawer).toHaveCount(0);
		await expect(node).toBeFocused();
	});

	test('20. 狭幅でも同じ経路でフォーカスが生きた要素へ戻る（#381 レビュー対応11回目）', async () => {
		await page.setViewportSize(NARROW_VIEWPORT);
		await treeToggle.click();
		await expect(treePane).toBeVisible();

		await groupNodeByName(page, GROUP_A).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '収集グループを再設定', exact: true }).click();
		const groupDrawer = page.getByRole('dialog', { name: `${GROUP_A} を編集` });
		await expect(groupDrawer).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(groupDrawer).toHaveCount(0);

		// 戻り先は「右クリックしたノード（退避パネルは開いたままなので生きている）」
		// か、それが `inert` 等で戻せないときのツリーのトグル。どちらにせよ
		// `<body>` には落ちない。
		expect(
			await page.evaluate(() => {
				const active = document.activeElement;
				if (!active || active === document.body) return false;
				const pane = document.getElementById('tags-tree-pane');
				const toggle = document.querySelector('[data-testid="tag-tree-toggle"]');
				return (!!pane && pane.contains(active)) || active === toggle;
			})
		).toBe(true);

		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('21. タグモニタでも 400px でツリーを開いて絞り込める', async () => {
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

	test('22. モーダルを閉じた直後（outro 中）の Esc でも、次の層が閉じる（#381 レビュー対応12回目）', async () => {
		// `Drawer`/`Modal` は `open=false` のあと outro（fade/fly）のあいだ DOM に
		// 残り、矩形も `visibility` も可視のまま。印（`data-layer-inactive`）を
		// 付けないと、その ~150ms は下の層が「上に層がある」と譲るのに閉じるものが
		// 無く、**Esc が無反応**になる。
		await page.goto('/tags');
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await treePane.getByRole('button', { name: 'PLC接続を追加' }).click();
		const createModal = page.getByRole('dialog', { name: '新規作成' });
		await expect(createModal).toBeVisible();

		// 1回目で Modal を閉じ、**待たずに**2回目を送る（outro の最中）。
		await page.keyboard.press('Escape');
		await page.keyboard.press('Escape');

		await expect(createModal).toHaveCount(0);
		await expect(treePane).toBeHidden();
	});

	test('23. 編集 Drawer からタグを削除してもフォーカスが body に落ちない（#381 レビュー対応12回目）', async () => {
		// 削除は「Drawer を閉じる」のと「行が一覧から消える」のが同じ更新で起きる。
		// DOM 更新の前に戻すと、消える直前の行へ戻してフォーカスが落ちる。
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_A);
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();
		const editDrawer = page.getByRole('dialog', { name: `${TAG_A} を編集` });
		await expect(editDrawer).toBeVisible();

		page.once('dialog', (dialog) => void dialog.accept());
		await editDrawer.getByRole('button', { name: '削除', exact: true }).click();
		await expect(editDrawer).toHaveCount(0);

		// 戻し先の行は消えているので、`focusFallback`（ツリーのトグル）へ。
		await expect
			.poll(() => page.evaluate(() => document.activeElement?.tagName ?? null))
			.not.toBe('BODY');
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('24. パネルの外へ飛ばされたフォーカスはトラップが引き戻す（#381 レビュー対応13回目）', async () => {
		// トラップがパネルの `keydown` だけだと、**プログラム的な `focus()` で
		// 一度外へ出たあとの Tab はパネルの外で起きる**ので届かない。document の
		// `focusin` で引き戻す（`focusTrap.ts`）。
		await page.goto('/tags');
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_B);
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click();
		const editDrawer = page.getByRole('dialog', { name: `${TAG_B} を編集` });
		await expect(editDrawer).toBeVisible();

		await page.evaluate(() => {
			document.querySelector<HTMLElement>('[data-testid="tag-tree-toggle"]')?.focus();
		});

		await expect
			.poll(() =>
				page.evaluate(() => {
					const panel = document.querySelector('[role="dialog"][aria-modal="true"]');
					const active = document.activeElement;
					return !!panel && !!active && panel.contains(active);
				})
			)
			.toBe(true);

		await page.keyboard.press('Escape');
		await expect(editDrawer).toHaveCount(0);
	});

	test('25. 開いた時点のフォーカスが body でも、閉じたら fallback へ戻る（#381 レビュー対応13回目）', async () => {
		// `<body>` を「生きた戻し先」と見なすと fallback が飛ばされ、閉じたあと
		// フォーカスがどこにも無いままになる（`focusRestore.ts`）。
		await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
		// クリックすると行にフォーカスが移ってしまうので、`<body>` にフォーカスが
		// 無い状態のままイベントだけ送る。
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).dispatchEvent('click');
		const editDrawer = page.getByRole('dialog', { name: `${TAG_B} を編集` });
		await expect(editDrawer).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(editDrawer).toHaveCount(0);
		// 戻し先（body）は拒否されるので `focusFallback`（ツリーのトグル）へ。
		await expect(treeToggle).toBeFocused();
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('26. メニューはフォーカスが外れたら閉じ、Esc は下の層へ届く（#381 レビュー対応13回目）', async () => {
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await groupNodeByName(page, GROUP_A).click({ button: 'right' });
		const menu = page.getByRole('menu', { name: '作成メニュー' });
		await expect(menu).toBeVisible();

		// フォーカスをメニューの外へ飛ばす（「見えているのにフォーカスは外」と
		// いう状態を作らせない）。
		await page.evaluate(() => {
			document.querySelector<HTMLElement>('[data-testid="tag-tree-toggle"]')?.focus();
		});
		await expect(menu).toHaveCount(0);

		// メニューが残っていないので、Esc は下の層（退避ツリー）へ届く。
		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('27. 退避したサイドバーの中にフォーカスを残さない（#381 レビュー対応12回目）', async () => {
		// 狭幅のサイドバーは `translateX(-100%)` で退避するだけだったので、閉じた
		// あとも中のナビ項目がフォーカスを受けられた（`inert` + `visibility: hidden`
		// で「無い」ことを明示した - `escLayering.ts` の層の約束・項目6）。あわせて
		// **畳む瞬間に中にフォーカスがあればヘッダーの ☰ へ逃がす**（`SplitPane` の
		// `focusFallback` と同じ役割。`inert` が付いた後ではブラウザが先に
		// フォーカスを外してしまうので、判定は DOM 更新の前に行っている）。
		await page.goto('/tags');
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		const navLink = page.getByRole('link', { name: 'タグモニタ' });
		await navLink.focus();
		await expect(navLink).toBeFocused();

		await page.keyboard.press('Escape');
		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();

		// 退避したサイドバーの中にも `<body>` にも残らない。
		await expect
			.poll(() =>
				page.evaluate(() => {
					const active = document.activeElement;
					if (!active || active === document.body) return false;
					const aside = document.querySelector('aside');
					return !(aside && aside.contains(active));
				})
			)
			.toBe(true);
		// 退避後のナビ項目はフォーカスを受けられない（`inert`）。
		await expect(navLink).toBeHidden();
	});

	test('28. 層が重なっているとき、引き戻すのは最上位の層（#381 レビュー対応14回目）', async () => {
		// 層の判定が「自分以外の可視な層」だった頃は、パレット(1000)と Drawer(900)
		// が重なると互いを「上」と誤認して**双方が引き戻しを諦め**、裏のページへ
		// フォーカスが抜けられた。判定を z 順対応にしたので、引き戻すのは
		// 最上位のパレットだけになる。
		await page.goto('/tags');
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_B);
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click();
		const editDrawer = page.getByRole('dialog', { name: `${TAG_B} を編集` });
		await expect(editDrawer).toBeVisible();

		await page.keyboard.press('Control+k');
		const palette = page.getByRole('dialog', { name: 'コマンドパレット' });
		await expect(palette).toBeVisible();

		await page.evaluate(() => {
			document.querySelector<HTMLElement>('[data-testid="tag-tree-toggle"]')?.focus();
		});

		await expect
			.poll(() =>
				page.evaluate(() => {
					const panel = document.querySelector('[role="dialog"][aria-label="コマンドパレット"]');
					const active = document.activeElement;
					return !!panel && !!active && panel.contains(active);
				})
			)
			.toBe(true);

		await page.keyboard.press('Escape');
		await expect(palette).toHaveCount(0);
		await page.keyboard.press('Escape');
		await expect(editDrawer).toHaveCount(0);
	});

	test('29. メニューが出ているあいだ Ctrl+K はパレットを開かない（#381 レビュー対応14回目）', async () => {
		// パレットとメニューは同じ z（1000）なので z 順では解けない - 同時に
		// 出さないのは呼び出し側の責務（`escLayering.ts`）。
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await groupNodeByName(page, GROUP_B).click({ button: 'right' });
		const menu = page.getByRole('menu', { name: '作成メニュー' });
		await expect(menu).toBeVisible();

		await page.keyboard.press('Control+k');
		const palette = page.getByRole('dialog', { name: 'コマンドパレット' });
		await expect(palette).toHaveCount(0);
		await expect(menu).toBeVisible();

		// メニューを閉じれば従来どおり開く。
		await page.keyboard.press('Escape');
		await expect(menu).toHaveCount(0);
		await page.keyboard.press('Control+k');
		await expect(palette).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(palette).toHaveCount(0);

		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('30. メニュー由来の戻し先は次の開閉へ持ち越さない（#381 レビュー対応14回目）', async () => {
		// メニューから開いた Drawer を閉じたあと、**別の経路（グリッド行）で開いた
		// Drawer の戻し先が消えた**とき、古いツリーノードへ飛ばない。
		await treeToggle.click();
		await groupNodeByName(page, GROUP_B).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '収集グループを再設定', exact: true }).click();
		const groupDrawer = page.getByRole('dialog', { name: `${GROUP_B} を編集` });
		await expect(groupDrawer).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(groupDrawer).toHaveCount(0);

		// ツリーを閉じてからグリッド行 → 削除（戻し先の行は消える）。
		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_B);
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click();
		const editDrawer = page.getByRole('dialog', { name: `${TAG_B} を編集` });
		await expect(editDrawer).toBeVisible();

		page.once('dialog', (dialog) => void dialog.accept());
		await editDrawer.getByRole('button', { name: '削除', exact: true }).click();
		await expect(editDrawer).toHaveCount(0);

		// 古いツリーノードではなく、ページ側の fallback（ツリーのトグル）へ。
		await expect(treeToggle).toBeFocused();
		await page.getByPlaceholder('名前・アドレスで検索').fill('');
	});

	test('31. ペイン内にフォーカスがあっても、サイドバーが先に閉じる（#381 レビュー対応15回目）', async () => {
		// サイドバー（710）は `dialog`/`menu` を名乗らないので、退避ツリー（610）から
		// 「上の層」として見えず、ペイン内の Esc がツリーを先に閉じていた。
		// 汎用マーカー `data-esc-layer` で層に入れたので、1層ずつ畳める。
		await page.goto('/tags');
		await treeToggle.click();
		await expect(treePane).toBeVisible();
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		await expect(page.getByRole('button', { name: 'メニューを閉じる', exact: true })).toBeVisible();

		// フォーカスを**ペインの中**へ置く（ここが以前の抜け道）。
		await treePane.getByRole('button', { name: 'PLC接続を追加' }).focus();

		await page.keyboard.press('Escape');
		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expect(treePane).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(treePane).toBeHidden();
	});

	test('32. 狭幅→広幅でトグルにフォーカスがあっても body に落ちない（#381 レビュー対応15回目）', async () => {
		// 狭幅のトグルは広幅への更新と同時にアンマウントされ、`focusFallback()` も
		// 同じトグルを返すので戻し先が無くなる。広幅で普通に使えるようになった
		// 左ペインの先頭要素へ渡す。
		await treeToggle.focus();
		await expect(treeToggle).toBeFocused();

		await page.setViewportSize({ width: 1280, height: 800 });
		await expect(treeToggle).toHaveCount(0);

		await expect
			.poll(() =>
				page.evaluate(() => {
					const active = document.activeElement;
					if (!active || active === document.body) return false;
					// 広幅では退避パネルの id は付かないので、ツリー本体で判定する。
					const tree = document.querySelector('[role="tree"]')?.closest('.pane-left');
					return !!tree && tree.contains(active);
				})
			)
			.toBe(true);

		await page.setViewportSize(NARROW_VIEWPORT);
	});

	test('33. 広幅で右クリックから接続を削除しても、フォーカスは body に落ちない（#381 レビュー対応16回目）', async () => {
		// `ConnectionDrawer`/`CollectionGroupDrawer` は削除後の再読込を待たずに
		// 閉じていたため、戻しが「消える予定のノード」へ向かい、直後の再読込で
		// フォーカスが `<body>` に落ちていた（広幅はツリーのトグルが無いので
		// 代替も無かった）。再読込を待ってから閉じ、広幅の fallback として
		// ツリーの先頭ノードを返すようにした。
		await page.setViewportSize({ width: 1280, height: 800 });
		await page.goto('/tags');
		await page.getByRole('tree').getByRole('button', { name: CONNECTION_NAME }).click({
			button: 'right'
		});
		await page.getByRole('menuitem', { name: '接続を削除', exact: true }).click();

		const connectionDrawer = page.getByRole('dialog', { name: `${CONNECTION_NAME} を編集` });
		await expect(connectionDrawer).toBeVisible();

		page.once('dialog', (dialog) => void dialog.accept());
		await connectionDrawer.getByRole('button', { name: '削除', exact: true }).click();
		await expect(connectionDrawer).toHaveCount(0);

		// 消えたノードでも `<body>` でもなく、ツリー内の生きた要素へ。
		await expect
			.poll(() =>
				page.evaluate(() => {
					const active = document.activeElement;
					if (!active || active === document.body) return false;
					const tree = document.querySelector('[role="tree"]');
					return !!tree && tree.contains(active);
				})
			)
			.toBe(true);

		await page.setViewportSize(NARROW_VIEWPORT);
	});
});
