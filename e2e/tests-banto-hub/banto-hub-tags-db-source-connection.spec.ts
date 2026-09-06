/**
 * S1（docs/banto-hub-external-db-design.md §3.1「案A: 既存3階層の流用」・
 * §4.1・§6-4）: `protocol: "postgres"`（DB Source）接続の SvelteKit/TS 側
 * 受け入れテスト。実 PostgreSQL には依存しない - 接続テスト自体
 * （`POST /api/plc-connections/{id}/test`）は
 * `ConnectionDrawer.svelte::runDbConnectionTest` を経由するボタンの存在・
 * 活性状態のみ確認し、実際にクリックして疎通させることはしない（CI には
 * まだ実 PostgreSQL が無い - 実装指示6「CI で実 PostgreSQL に依存しない」）。
 *
 * 固定する内容:
 * 1. 作成ウィザードでプロトコルを postgres に切り替えると、ポートが 5432
 *    へ追従し、database/username が必須になり（未入力では「次へ」に進めず、
 *    送信すると required エラーが出る）、unitId/ワード順/シミュレーション
 *    フィールドが消える。
 * 2. 作成できたら、ツリーに DB バッジが付いた状態で現れる。
 * 3. 再設定 Drawer を開くと「パスワード: 設定済み」（作成時に入力した
 *    パスワードがある）と出て、パスワード欄は空欄のまま（絶対にプリ
 *    フィルしない）。
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

		// プロトコルを postgres へ切り替える - ポートはまだ既定値（502、
		// modbus-tcp の既定）のままなので postgres の既定 5432 へ追従する
		// （`plcConnectionForm.ts::onProtocolChange`と同じ仕組み）。
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

		// 確認ステップ: database/username/パスワードの有無を表示し、接続
		// テストは「保存後にテストできます」のヒント付きで無効。
		await expect(wizard.getByText('appdb')).toBeVisible();
		await expect(wizard.getByText('appuser')).toBeVisible();
		await expect(wizard.getByText('保存後にテストできます')).toBeVisible();
		await expect(wizard.getByRole('button', { name: '接続テスト' })).toBeDisabled();

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
