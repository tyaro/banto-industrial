/**
 * #332 chronogazer 分（Hub 接続の設定カテゴリ）の実 DOM 固定。
 *
 * banto-hub との同時起動はこのリポジトリの E2E に前例が無く、今回は作って
 * いない。ここで固定するのは **Hub 無しで検証できる範囲**だけ:
 * - admin のナビに `Hub接続` が出て、初期状態が「未設定」であること。
 * - 到達不能な URL で「接続」すると **「Hubに到達できません」**という
 *   専用の状態になること（受入条件: エラーを「タグ0件」に潰さない）。
 * - そのとき設定は**保存されない**こと（「切断」が出ず、再読込で「未設定」
 *   に戻る）。
 * - **購読ブロック**（#383 段階1）が未設定でも「停止」を理由付きで出し、
 *   値の表を出さないこと。到達不能な Hub でも例外を出さないこと。
 *   `banto-serve` は OS キーリングを持てないので購読は常に張れない。
 * - 非 admin では `Hub接続` が見えず、`/settings/hub` への直接遷移が先頭の
 *   可視カテゴリへ弾かれること（`guardCategory`）。
 * -（観点別レビュー P2-B の回帰固定）**未保存のタグ選択が「一覧を更新」で
 *   黙って消えない**こと（#378「未保存の入力を黙って捨てない」）。修正前は
 *   `applyView()` がサーバーの `selectedTags` で無条件に上書きしており、
 *   チェックを変えてから隣の「一覧を更新」を押すと確認も警告もなく元に
 *   戻っていた。
 *
 *   この 1 本だけ `page.route` で `GET /api/hub`・`POST /api/hub/refresh` を
 *   差し替える（作法は `e2e/tests/tags.spec.ts` の `page.route` ゲートに
 *   倣う）。タグのチェックボックスは接続状態が `connected` のときにしか
 *   描かれず、`banto-serve` は OS キーリングを持てない（`UnavailableKeyStore`）
 *   ため、実サーバーだけでは `connected` に到達する手段が無い - この画面で
 *   Hub を実際に立てる仕組みはこのリポジトリの E2E に無い（上の doc
 *   comment 参照）。差し替えるのは一覧と選択を返す 2 本だけで、購読の
 *   ポーリング（`GET /api/hub/subscription`）は実サーバーのまま流す。
 * -（オーナーレビュー P2 の回帰固定 3 本、test 9〜11）**切断に失敗しても
 *   未保存の注記が消えない**こと、**別の Hub に繋ぎ直したら旧 Hub のタグ名が
 *   保存の payload に混ざらない**こと、**購読の読み取りが応答しないときに
 *   「状態を取得できていません」に切り替わる**こと。最後の 1 本は `page.route`
 *   で要求を**握ったまま応答しない**（即座に失敗するモックでは、ポーリングが
 *   ループごと止まる欠陥を再現できない）。
 *
 * 「接続済み」「連携が必要」など Hub を実際に必要とする状態は
 * `crates/banto-hub-bootstrap` のモックサーバー付きテスト（27 本）が固定
 * している。
 *
 * ファイル名について: `smoke.spec.ts` が初回セットアップ（管理者アカウント
 * 作成）を実 DOM で行うため、このファイルは辞書順でそれより後でなければ
 * ならない（`playwright.config.ts` は `workers: 1`/`fullyParallel: false`
 * で `testDir` 配下をファイル名の辞書順に実行する。`user-settings-routes
 * .spec.ts` の doc comment にある罠と同じ）。`user-settings-hub` は
 * `smoke` より後、`user-settings-routes` より前に入る（`h` < `r`）。
 * 後続の `user-settings-routes.spec.ts` はここで作る閲覧者アカウントの
 * 有無に依存しない（admin でログインしてナビを見るだけ）。
 */
import { expect, test, type Page, type Route } from '@playwright/test';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

// このスペックが作る閲覧者アカウント（非 admin の可視性を確かめるため）。
const VIEWER_USERNAME = 'e2e-hub-viewer';
const VIEWER_PASSWORD = 'E2eViewerPass1';

// ポート 1 は待ち受けが無く、接続が即座に拒否される - 「到達不能」を
// タイムアウト待ちなしで再現できる。
const UNREACHABLE_HUB = 'http://127.0.0.1:1';

async function login(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード').fill(password);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

test.describe.serial('chronogazer Hub接続の設定カテゴリ', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await login(page, ADMIN_USERNAME, ADMIN_PASSWORD);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. admin のカテゴリナビに Hub接続 が並ぶ', async () => {
		await page.goto('/settings');
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await expect(nav.getByRole('link', { name: 'Hub接続' })).toBeVisible();
	});

	test('2. Hub接続へ遷移すると見出しが出て、初期状態は「未設定」', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: 'Hub接続' }).click();
		await expect(page).toHaveURL(/\/settings\/hub$/);
		await expect(page.getByRole('heading', { level: 2, name: 'Hub接続' })).toBeVisible();
		await expect(page.getByText('状態: 未設定')).toBeVisible();
		// 未設定のうちは「切断」を出さない（消すものが無い）。
		await expect(page.getByRole('button', { name: '切断' })).toHaveCount(0);
	});

	test('3. 到達不能なURLで「接続」すると「Hubに到達できません」になり、設定は保存されない', async () => {
		await page.getByLabel('接続先URL').fill(UNREACHABLE_HUB);
		await page.getByRole('button', { name: '接続' }).click();

		await expect(page.getByText('状態: Hubに到達できません')).toBeVisible();
		// 失敗した接続は記録を残さないので「切断」は出ない。
		await expect(page.getByRole('button', { name: '切断' })).toHaveCount(0);

		// 再読込すると保存済み設定が無いことがそのまま出る。
		await page.reload();
		await expect(page.getByText('状態: 未設定')).toBeVisible();
	});

	// #383 段階1: 購読ブロック。`banto-serve` は OS キーリングを持てない
	// （`UnavailableKeyStore`）ので購読は決して張れず、常に「停止」＋理由に
	// なる。ここで確かめたいのは「購読が張れなくても画面が壊れず、値の表を
	// 出さない」こと - 購読の失敗が接続設定の 6 状態を汚さないという規律を
	// 実 DOM 側から固定する。
	test('4. 未設定でも購読ブロックは「停止」を理由付きで出し、値の表は出さない', async () => {
		await page.goto('/settings/hub');
		await expect(page.getByText('状態: 未設定')).toBeVisible();
		await expect(page.getByRole('heading', { level: 3, name: '購読' })).toBeVisible();
		await expect(page.getByText('購読: 停止')).toBeVisible();
		await expect(page.getByText('値を受信していません。')).toBeVisible();
		// 値の表は Live のときだけ。停止中に古い値や空の表を出さない。
		await expect(page.getByRole('columnheader', { name: '品質' })).toHaveCount(0);
	});

	test('5. 到達不能なHubへ接続を試みても購読ブロックは例外を出さない', async () => {
		await page.getByLabel('接続先URL').fill(UNREACHABLE_HUB);
		await page.getByRole('button', { name: '接続' }).click();

		await expect(page.getByText('状態: Hubに到達できません')).toBeVisible();
		// 接続が失敗しても購読ブロックは「停止」のまま残る（消えない・
		// 例外で画面が落ちない）。
		await expect(page.getByText('購読: 停止')).toBeVisible();
		await expect(page.getByText('値を受信していません。')).toBeVisible();
	});

	test('6. 閲覧者アカウントを作成する（次のテストの前提）', async () => {
		await page.goto('/users');
		// 入力欄は「新規作成」セクションに限定して取る。一覧のグリッドには
		// 同名の列見出し（「ユーザー名の絞り込み」等）があり、ページ全体を
		// 対象にすると `getByLabel` が strict mode 違反になる。
		const createForm = page.locator('section.create');
		await expect(createForm.getByRole('heading', { level: 3, name: '新規作成' })).toBeVisible();
		await createForm.getByLabel('ユーザー名').fill(VIEWER_USERNAME);
		await createForm.getByLabel('パスワード（8文字以上）').fill(VIEWER_PASSWORD);
		await createForm.getByLabel('表示名').fill('E2E閲覧者');
		await createForm.getByLabel('ロール').selectOption('viewer');
		await createForm.getByRole('button', { name: '作成' }).click();
		await expect(page.locator('section.list').getByText(VIEWER_USERNAME).first()).toBeVisible();
	});

	test('7. 非 admin には Hub接続 が見えず、直接遷移は先頭の可視カテゴリへ弾かれる', async ({
		browser
	}) => {
		const viewerPage = await browser.newPage();
		try {
			await login(viewerPage, VIEWER_USERNAME, VIEWER_PASSWORD);

			await viewerPage.goto('/settings');
			const nav = viewerPage.getByRole('navigation', { name: '設定のカテゴリ' });
			await expect(nav.getByRole('link', { name: '外観' })).toBeVisible();
			await expect(nav.getByRole('link', { name: 'Hub接続' })).toHaveCount(0);

			await viewerPage.goto('/settings/hub');
			await expect(viewerPage).toHaveURL(/\/settings\/appearance$/);
			await expect(viewerPage.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
		} finally {
			await viewerPage.close();
		}
	});

	// 観点別レビュー P2-B の回帰固定。差し替えの理由はこのファイルの doc
	// comment 参照（`connected` は実サーバーだけでは作れない）。
	test('8. 未保存のタグ選択は「一覧を更新」で黙って消えず、明示的に捨てる導線が出る', async () => {
		const SELECTED_TAG = 'line1.temp';
		const UNSELECTED_TAG = 'line1.press';
		// `HubView`（`chronogazer_core::hub::HubView`）と同じ形。サーバー側の
		// 選択は `SELECTED_TAG` だけ。
		const hubView = {
			status: { state: 'connected', tagCount: 2 },
			endpoint: 'http://127.0.0.1:3100',
			keyName: 'chronogazer-e2e',
			selectedTags: [SELECTED_TAG],
			tags: [
				{
					externalName: SELECTED_TAG,
					name: 'temp',
					dataType: 'f32',
					unit: 'degC',
					tagKind: 'plc'
				},
				{
					externalName: UNSELECTED_TAG,
					name: 'press',
					dataType: 'f32',
					unit: 'kPa',
					tagKind: 'plc'
				}
			],
			subscription: {
				state: 'stopped',
				reason: '購読していません。',
				subscribedCount: 0,
				unresolved: [],
				unsupported: [],
				lastError: null,
				lastValueAt: null,
				values: []
			}
		};

		await page.route('**/api/hub', async (route) => {
			if (route.request().method() === 'GET') {
				await route.fulfill({ json: hubView });
				return;
			}
			await route.continue();
		});
		await page.route('**/api/hub/refresh', async (route) => {
			await route.fulfill({ json: hubView });
		});

		try {
			await page.goto('/settings/hub');
			await expect(page.getByText('状態: 接続済み（タグ2件）')).toBeVisible();

			const selectedBox = page.getByRole('checkbox', { name: SELECTED_TAG });
			const unselectedBox = page.getByRole('checkbox', { name: UNSELECTED_TAG });
			await expect(selectedBox).toBeChecked();
			await expect(unselectedBox).not.toBeChecked();

			// 未保存の変更を作る（保存はしない）。
			await unselectedBox.check();
			await expect(unselectedBox).toBeChecked();

			// 「選択を保存」の隣にある「一覧を更新」を押す。修正前はここで
			// チェックがサーバーの内容（SELECTED_TAG だけ）に戻っていた。
			await page.getByRole('button', { name: '一覧を更新' }).click();

			await expect(unselectedBox).toBeChecked();
			await expect(selectedBox).toBeChecked();
			await expect(page.getByText('選択に未保存の変更があります')).toBeVisible();

			// 捨てるときは明示的に（黙って捨てない代わりの導線）。
			await page.getByRole('button', { name: 'サーバーの内容に戻す' }).click();
			await expect(unselectedBox).not.toBeChecked();
			await expect(selectedBox).toBeChecked();
			await expect(page.getByText('選択に未保存の変更があります')).toHaveCount(0);
		} finally {
			await page.unroute('**/api/hub/refresh');
			await page.unroute('**/api/hub');
		}
	});

	// --- オーナーレビュー P2 の回帰固定 ---------------------------------------

	/** test 8 と同じ `HubView` の形。接続先とタグだけ差し替えて使う。 */
	function hubViewFor(endpoint: string, tagNames: string[], selectedTags: string[]) {
		return {
			status: { state: 'connected', tagCount: tagNames.length },
			endpoint,
			keyName: 'chronogazer-e2e',
			selectedTags,
			tags: tagNames.map((externalName) => ({
				externalName,
				name: externalName,
				dataType: 'f32',
				unit: null,
				tagKind: 'plc'
			})),
			subscription: {
				state: 'stopped',
				reason: '購読していません。',
				subscribedCount: 0,
				unresolved: [],
				unsupported: [],
				lastError: null,
				lastValueAt: null,
				values: []
			}
		};
	}

	// P2-1: 未保存フラグを `await` の前に降ろしていたため、切断が失敗すると
	// エラーだけが出て注記が消え、次の「一覧を更新」でサーバーの選択に黙って
	// 上書きされた（この PR で塞いだはずの穴が失敗経路に残っていた）。
	test('9. 切断に失敗しても未保存の注記は残り、次の「一覧を更新」で選択が消えない', async () => {
		const SELECTED_TAG = 'line1.temp';
		const UNSELECTED_TAG = 'line1.press';
		const view = hubViewFor(
			'http://127.0.0.1:3100',
			[SELECTED_TAG, UNSELECTED_TAG],
			[SELECTED_TAG]
		);

		await page.route('**/api/hub', async (route) => {
			const method = route.request().method();
			if (method === 'GET') {
				await route.fulfill({ json: view });
				return;
			}
			if (method === 'DELETE') {
				// 切断の失敗（Hub 側ではなくローカルの保存先が落ちた場合など）。
				await route.fulfill({
					status: 500,
					json: { kind: 'other', message: '切断に失敗しました' }
				});
				return;
			}
			await route.continue();
		});
		await page.route('**/api/hub/refresh', async (route) => {
			await route.fulfill({ json: view });
		});

		try {
			await page.goto('/settings/hub');
			await expect(page.getByText('状態: 接続済み（タグ2件）')).toBeVisible();

			const unselectedBox = page.getByRole('checkbox', { name: UNSELECTED_TAG });
			await unselectedBox.check();
			await expect(page.getByText('選択に未保存の変更があります')).toBeVisible();

			// 切断は失敗する。エラーは出るが、未保存の状態は落ちてはいけない。
			await page.getByRole('button', { name: '切断' }).click();
			await expect(page.getByText('切断に失敗しました')).toBeVisible();
			await expect(page.getByText('選択に未保存の変更があります')).toBeVisible();
			await expect(unselectedBox).toBeChecked();

			// 修正前はここでサーバーの選択（SELECTED_TAG だけ）に戻っていた。
			await page.getByRole('button', { name: '一覧を更新' }).click();
			await expect(unselectedBox).toBeChecked();
			await expect(page.getByText('選択に未保存の変更があります')).toBeVisible();
		} finally {
			await page.unroute('**/api/hub/refresh');
			await page.unroute('**/api/hub');
		}
	});

	// P2-2: 下書きは接続先ごとのもの。別の Hub に繋ぎ直すと `tags` だけが
	// 入れ替わり、画面に出ていない旧 Hub のタグ名が保存の payload に混入した。
	test('10. 別のHubに接続すると旧Hubの下書きは破棄され、保存の payload に混ざらない', async () => {
		const HUB_A = 'http://127.0.0.1:3100';
		const HUB_B = 'http://127.0.0.1:3200';
		const TAG_A = 'hubA.temp';
		const TAG_B = 'hubB.flow';
		const viewA = hubViewFor(HUB_A, [TAG_A], []);
		const viewB = hubViewFor(HUB_B, [TAG_B], [TAG_B]);
		let savedTags: unknown = null;

		await page.route('**/api/hub', async (route) => {
			if (route.request().method() === 'GET') {
				await route.fulfill({ json: viewA });
				return;
			}
			await route.continue();
		});
		await page.route('**/api/hub/connect', async (route) => {
			await route.fulfill({ json: viewB });
		});
		await page.route('**/api/hub/selected-tags', async (route) => {
			savedTags = (route.request().postDataJSON() as { tags?: unknown }).tags;
			await route.fulfill({ status: 204, body: '' });
		});

		try {
			await page.goto('/settings/hub');
			const tagABox = page.getByRole('checkbox', { name: TAG_A });
			await tagABox.check();
			await expect(page.getByText('選択に未保存の変更があります')).toBeVisible();

			// Hub B へ繋ぎ直す。
			await page.getByLabel('接続先URL').fill(HUB_B);
			await page.getByRole('button', { name: '接続' }).click();

			// 一覧は Hub B のものに入れ替わり、破棄したことが画面に出る。
			await expect(page.getByRole('checkbox', { name: TAG_B })).toBeChecked();
			await expect(page.getByRole('checkbox', { name: TAG_A })).toHaveCount(0);
			await expect(page.getByText('未保存だった選択は破棄しました')).toBeVisible();
			await expect(page.getByText('選択に未保存の変更があります')).toHaveCount(0);

			await page.getByRole('button', { name: '選択を保存' }).click();
			await expect(page.getByText('選択したタグ（1件）を保存しました。')).toBeVisible();
			// 修正前は Hub A のタグ名がそのまま Hub B の保存に混ざっていた。
			expect(savedTags).toEqual([TAG_B]);
		} finally {
			await page.unroute('**/api/hub/selected-tags');
			await page.unroute('**/api/hub/connect');
			await page.unroute('**/api/hub');
		}
	});

	// P2-3: いちばん大事な 1 本。`getHubSubscription()` にはどちらの経路にも
	// 上限が無く、**TCP は繋がるが応答が返らない**相手だと `catch` に入らない
	// ため失敗が数えられず、`pollThenSchedule()` の次の予約も行われずループごと
	// 止まった（ユーザーがボタンを押すまで「受信中」＋最後の値が永久に残る）。
	// 即座に失敗するモックではこの欠陥を再現できないので、ここでは要求を
	// **握ったまま応答しない**。
	test('11. 購読の読み取りが応答しないと「状態を取得できていません」に切り替わる', async () => {
		/** 握った要求を最後に解放するための resolver。 */
		const release: (() => void)[] = [];
		// 解放後に届いた要求まで握ると、後片付けの `goto` が止まる。
		let released = false;

		await page.route('**/api/hub/subscription', async (route: Route) => {
			if (!released) await new Promise<void>((resolve) => release.push(resolve));
			// テストの終わりに解放する。ここまで応答しない = 無応答の再現。
			await route.abort().catch(() => {});
		});

		try {
			await page.goto('/settings/hub');
			// 接続状態（`GET /api/hub`）は実サーバーのまま取れるので、購読ブロック
			// 自体は描かれる。嘘のまま固まらず切り替わることを見る。
			await expect(page.getByRole('heading', { level: 3, name: '購読' })).toBeVisible();
			// 見出しは既存の状態名を残したまま「取得できていない」ことだけを
			// 添える。上限 4 秒 × 2 回 + ポーリング間隔なので、最悪でも 10 秒強。
			await expect(page.getByText('停止（状態を取得できていません）')).toBeVisible({
				timeout: 30_000
			});
			// いつの表示なのかを添える注記（値の表は消さない）。
			await expect(page.getByText('購読状態を取得できていません。下の表示は')).toBeVisible();
		} finally {
			released = true;
			for (const resolve of release) resolve();
			// 次のスペックに無応答のポーリングを持ち越さない。
			await page.goto('/settings/appearance');
			await page.unroute('**/api/hub/subscription');
		}
	});
});
