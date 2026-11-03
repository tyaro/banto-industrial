/**
 * S1（docs/banto-hub-external-db-design.md §3.1「案A: 既存3階層の流用」・
 * §4.1・§6-4）: `protocol: "postgres"`（DB Source）接続の SvelteKit/TS 側
 * 受け入れテスト。実 PostgreSQL には依存しない。保存前テストの要求、
 * 応答を待つ間の操作の制限、成功/失敗の表示、保存前に行を作らないことを確認する。
 * PostgreSQL 自体は Rust のプロトコル試験と PostgreSQL CI で検証する。
 *
 * 固定する内容:
 * 1. 作成ウィザードでプロトコルを postgres に切り替えると、ポートが 5432
 *    へ追従し、database/username が必須になり（未入力では「次へ」に進めず、
 *    送信すると required エラーが出る）、unitId/ワード順/シミュレーション
 *    フィールドが消える。
 * 2. 作成できたら、ツリーに DB バッジが付いた状態で現れる。
 * 3. 再設定 Drawer を開くと「パスワード: 設定済み」（作成時に入力した
 *    パスワードがある）と出て、パスワード欄は空欄のまま（絶対にプリ
 *    フィルしない）。接続先を変えて保存した場合は、422 の項目エラーが
 *    パスワード欄のそばに出る。
 * 4. **S3（docs/banto-hub-external-db-design.md §7 row S3）で更新**:
 *    「収集グループを作成」メニュー項目は S1 時点では無効化されていた
 *    （実装指示4「サーバーの422だけに頼らない」）が、S2/S2b で DB Source
 *    本体が実装されたため S3 でその制限を撤去した - この項目は常時有効に
 *    戻り、クリックすると収集グループ作成ウィザードが開くことを固定する
 *    （`tagTreeContextMenu.ts`の`resolveTreeContextMenuItems`参照）。
 *
 * `banto-hub-tags-tree-context-menu.spec.ts` と同じパターン: 別
 * `describe.serial` ブロック、前提データは `page.request` で直接 REST を
 * 叩いて作る/消す。同期点はトースト文言ではなく`page.waitForResponse`
 * （クリックより前に張る - 記憶メモ「toastをsyncポイントに使わない」の
 * 教訓）。ファイル名は `banto-hub-smoke.spec.ts` より辞書順で後。
 */
import { expect, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const DB_CONN = `e2e-dbsrc-conn-${RUN_ID}`;

interface ConnectionRow {
	id: number;
	name: string;
	protocol: string;
}

function escapeRegExp(value: string): string {
	return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * `banto-hub-auth.ts::groupNodeByName`と同じ理由: ツリーの接続ノードの
 * アクセシブル名は、DB Source 接続では「名前 + DB バッジ」の合成
 * （`ConnectionTree.svelte`の`.label`/`.badge`スパンがどちらもアクセシブル
 * 名に含まれる - アイコンだけ`aria-hidden`で除外される）になるため、
 * `exact: true`の完全一致は成立しない。名前の直後が空白または文字列末尾
 * であることまで確認して、他の接続名の前方一致と衝突しないようにする。
 */
function connectionNodeByName(page: Page, name: string): Locator {
	return page
		.getByRole('tree')
		.getByRole('button', { name: new RegExp(`^${escapeRegExp(name)}( |$)`) });
}

async function cleanupFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as ConnectionRow[];
		for (const c of connections.filter((c) => c.name === DB_CONN)) {
			await request.delete(`/api/plc-connections/${c.id}`, { headers });
		}
	}
}

test.describe
	.serial('banto-hub postgres（DB Source）接続の作成・ツリー表示・グループ作成（S1/S3）', () => {
	let adminPage: Page;
	let adminHeaders: Record<string, string>;

	test.beforeAll(async ({ browser }) => {
		adminPage = await browser.newPage();
		await adminPage.goto('/login');
		const adminToken = await fetchAuthToken(adminPage.request);
		await injectAuthToken(adminPage, adminToken);
		adminHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${adminToken}` };

		await cleanupFixtures(adminPage.request, adminHeaders);
	});

	test.afterAll(async () => {
		await cleanupFixtures(adminPage.request, adminHeaders);
		await adminPage.close();
	});

	test('1. 作成ウィザード: postgres へ切り替えるとポートが5432に追従し、unitId/ワード順/シミュレーションが消え、database/usernameが必須になる', async () => {
		await adminPage.goto('/tags');
		await adminPage.getByRole('button', { name: 'PLC接続を追加', exact: true }).click();

		const wizard = adminPage.getByRole('dialog', { name: '新規作成', exact: true });
		await expect(wizard).toBeVisible();
		await wizard.locator('#connection-name').fill(DB_CONN);
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();

		// プロトコルを postgres へ切り替える - ポートは postgres の既定 5432
		// （先頭のドライバ SLMP の既定 5007 とは別。ホストの接続の欄はスキーマに載らない）。
		await wizard.getByLabel('プロトコル').selectOption({ value: 'postgres' });
		await expect(wizard.getByLabel('ポート')).toHaveValue('5432');

		// postgres 専用フィールドが出て、PLC専用フィールドが消える。
		await expect(wizard.getByLabel('データベース')).toBeVisible();
		await expect(wizard.getByLabel('ユーザー名')).toBeVisible();
		await expect(wizard.getByLabel('ユニットID')).toHaveCount(0);
		await expect(wizard.getByText('シミュレーションモード')).toHaveCount(0);

		await wizard.getByLabel('ホスト').fill('10.0.0.5');

		// database/username が未入力のままでは「次へ」に進めない
		// （`canAdvanceFromStep2`、実装指示2のクライアント側検証）。
		await expect(wizard.getByRole('button', { name: '次へ', exact: true })).toBeDisabled();

		await wizard.getByLabel('データベース').fill('appdb');
		await wizard.getByLabel('ユーザー名').fill('appuser');
		// `exact: true`は使わない - このラベルはヒント文言
		// 「空欄のままでも作成できます（後から設定できます）。」も自身の
		// アクセシブル名に含む（`<label>`が入力欄とヒント`<span>`の両方を
		// 包んでいるため、`getByLabel`の照合対象はラベル全体のテキスト）。
		await wizard.getByLabel('パスワード').fill('hunter2');
		await expect(wizard.getByRole('button', { name: '次へ', exact: true })).toBeEnabled();

		await wizard.getByRole('button', { name: '次へ', exact: true }).click();

		// #1010: testing in step 3 must send current fields without persisting.
		await expect(wizard.getByText('appdb')).toBeVisible();
		await expect(wizard.getByText('appuser')).toBeVisible();
		const probeUrl = '**/api/plc-connections/test';
		let failProbe = false;
		let probes = 0;
		let releaseProbe!: () => void;
		const heldProbe = new Promise<void>((resolve) => {
			releaseProbe = resolve;
		});
		await adminPage.route(probeUrl, async (route) => {
			expect(route.request().postDataJSON()).toEqual({
				protocol: 'postgres',
				simulation: false,
				settings: {
					host: '10.0.0.5',
					port: 5432,
					database: 'appdb',
					username: 'appuser',
					password: 'hunter2'
				}
			});
			probes++;
			if (probes === 1) await heldProbe;
			await route.fulfill({
				json: failProbe
					? { ok: false, elapsedMs: 1, error: { kind: 'protocol', message: '試験用の接続エラー' } }
					: { ok: true, elapsedMs: 1, error: null }
			});
		});
		const probeButton = wizard.getByRole('button', { name: '接続テスト', exact: true });
		await expect(probeButton).toBeEnabled();
		const startedProbe = adminPage.waitForRequest((request) =>
			request.url().endsWith('/api/plc-connections/test')
		);
		let closeConfirmed = false;
		const onDialog = (dialog: import('@playwright/test').Dialog): void => {
			closeConfirmed = true;
			void dialog.dismiss();
		};
		adminPage.on('dialog', onDialog);
		try {
			await probeButton.click();
			await startedProbe;
			await expect(wizard.getByRole('button', { name: '戻る', exact: true })).toBeDisabled();
			await expect(wizard.getByRole('button', { name: '作成', exact: true })).toBeDisabled();
			await wizard.getByRole('button', { name: '閉じる', exact: true }).click();
			await expect(wizard).toBeVisible();
			await adminPage.keyboard.press('Escape');
			await expect(wizard).toBeVisible();
			await adminPage.mouse.click(1, 1);
			await expect(wizard).toBeVisible();
			expect(closeConfirmed).toBe(false);
		} finally {
			adminPage.off('dialog', onDialog);
			releaseProbe(); // Resolve before unroute, even when an assertion fails.
		}
		await expect(wizard.getByText('接続成功', { exact: true })).toBeVisible();
		failProbe = true;
		await probeButton.click();
		await expect(wizard.getByText('試験用の接続エラー', { exact: true })).toBeVisible();
		expect(probes).toBe(2);
		const beforeSave = await adminPage.request.get('/api/plc-connections', {
			headers: adminHeaders
		});
		expect(
			((await beforeSave.json()) as ConnectionRow[]).some(
				(connection) => connection.name === DB_CONN
			)
		).toBe(false);
		await adminPage.unroute(probeUrl);
		// Back clears old outcomes before any input can be changed/re-tested.
		await wizard.getByRole('button', { name: '戻る', exact: true }).click();
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();
		await expect(wizard.getByText('試験用の接続エラー', { exact: true })).toHaveCount(0);

		const created = adminPage.waitForResponse(
			(r) => r.url().includes('/api/plc-connections') && r.request().method() === 'POST'
		);
		await wizard.getByRole('button', { name: '作成', exact: true }).click();
		await created;

		await expect(adminPage.getByText('作成しました')).toBeVisible();
	});

	test('2. ツリーは postgres 接続を DB バッジ付きで表示する', async () => {
		await adminPage.goto('/tags');
		const node = connectionNodeByName(adminPage, DB_CONN);
		await expect(node).toBeVisible();
		await expect(node).toContainText('DB');
	});

	test('3. 再設定 Drawer: 「パスワード: 設定済み」と出て、パスワード欄はプリフィルされない', async () => {
		await adminPage.goto('/tags');
		const node = connectionNodeByName(adminPage, DB_CONN);
		await node.click({ button: 'right' });
		await adminPage.getByRole('menuitem', { name: '接続を再設定', exact: true }).click();

		const drawer = adminPage.getByRole('dialog', { name: `${DB_CONN} を編集`, exact: true });
		await expect(drawer).toBeVisible();
		await expect(drawer.getByText('パスワード: 設定済み')).toBeVisible();
		await expect(drawer.getByLabel('新しいパスワード（変更する場合のみ入力）')).toHaveValue('');
		await expect(drawer.getByLabel('データベース')).toHaveValue('appdb');
		await expect(drawer.getByLabel('ユーザー名')).toHaveValue('appuser');

		// 保存済み接続なので接続テストボタンは有効（実際にクリックはしない
		// - CI に実 PostgreSQL が無いため、ボタンの活性状態だけ固定する）。
		await expect(drawer.getByRole('button', { name: '接続テスト' })).toBeEnabled();

		await adminPage.keyboard.press('Escape');
	});

	test('再設定 Drawer: 接続先変更に必要なパスワードのエラーを入力欄のそばに表示する', async () => {
		await adminPage.goto('/tags');
		await connectionNodeByName(adminPage, DB_CONN).click({ button: 'right' });
		await adminPage.getByRole('menuitem', { name: '接続を再設定', exact: true }).click();

		const drawer = adminPage.getByRole('dialog', { name: `${DB_CONN} を編集`, exact: true });
		await drawer.getByLabel('ホスト', { exact: true }).fill('10.0.0.51');
		const saved = adminPage.waitForResponse(
			(r) => r.url().includes('/api/plc-connections/') && r.request().method() === 'PUT'
		);
		await drawer.getByRole('button', { name: '保存', exact: true }).click();
		expect((await saved).status()).toBe(422);
		const passwordError = drawer
			.getByLabel('パスワードを消去する', { exact: true })
			.locator('..')
			.locator('~ span.err');
		await expect(passwordError).toHaveText(
			'接続先を変更するには、パスワードを入力するか明示的に消去してください。'
		);
	});

	test('4. S3: 「収集グループを作成」は常時有効になり、クリックすると作成ウィザードが開く', async () => {
		await adminPage.goto('/tags');
		const node = connectionNodeByName(adminPage, DB_CONN);
		await node.click({ button: 'right' });

		const menu = adminPage.getByRole('menu', { name: '作成メニュー' });
		await expect(menu).toBeVisible();
		const createGroupItem = menu.getByRole('menuitem', { name: '収集グループを作成', exact: true });
		await expect(createGroupItem).toBeVisible();
		// S3: S1 の disabled ガードは撤去済み（`tagTreeContextMenu.ts`参照）
		// - `aria-disabled`/`title` のどちらも付かない。
		await expect(createGroupItem).not.toHaveAttribute('aria-disabled', 'true');
		await expect(createGroupItem).not.toHaveAttribute('title');

		// 接続そのものの再設定・削除は引き続き禁止されない（予約接続では
		// ない通常の接続なので）。
		await expect(menu.getByRole('menuitem', { name: '接続を再設定', exact: true })).toBeVisible();
		await expect(menu.getByRole('menuitem', { name: '接続を削除', exact: true })).toBeVisible();

		await createGroupItem.click();
		await expect(adminPage.getByRole('dialog', { name: '新規作成', exact: true })).toBeVisible();
		await adminPage.keyboard.press('Escape');
	});
});
