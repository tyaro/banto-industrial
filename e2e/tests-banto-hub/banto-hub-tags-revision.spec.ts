/**
 * タグ更新の楽観的ロック（T18-1 続き、TAG-UX-C 4点目の前半、
 * docs/banto-hub-desktop-plan.md §9.4「revision / ETag で後勝ち上書きを
 * 防ぐ」）の実 DOM 受け入れテスト。**差分表示 UI（フィールド単位の並列
 * 比較）は本 PR のスコープ外** — ここで確認するのは「他セッション更新を
 * 黙って上書きしない」（検出 + サーバー最新値での上書き）のみ。
 *
 * `banto-hub-tags-busy.spec.ts`/`banto-hub-tags-form.spec.ts` と同じ
 * パターン: 別 `describe.serial` ブロック（別 `page`）、認証・前提データ
 * （PLC接続・収集グループ・タグ）は `page.request` で直接 REST を叩いて
 * 作る。競合は「別経路（`page.request.put`、UI を経由しない別クライアント
 * 相当）が UI より先に同じタグを更新して revision を進める」ことで作る -
 * UI 側の編集フォームは古い revision を掴んだまま保存を試みる。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-revision-plc';
const GROUP_NAME = 'e2e-revision-group';
const TAG_NAME = 'e2e-revision-tag';

/** 他クライアント（UI を経由しない別経路）が確定させる単位。 */
const EXTERNAL_UNIT = 'external-kPa';
/** UI 側がフォームで入力しようとする単位（サーバーには反映されないはず）。 */
const UI_ATTEMPTED_UNIT = 'ui-should-not-win';

interface TagResponse {
	id: number;
	unit: string | null;
	revision: number;
}

test.describe.serial('banto-hub タグ更新の楽観的ロック (TAG-UX-C)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let tagId: number;
	let groupId: number;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ: シミュレーションモードの PLC接続 + 収集グループ +
		// タグ1件（実 PLC/実ネットワークへは繋がない）。新規行は
		// `TagService::create` により revision=1 で始まる
		// （`crates/banto-tags/src/tag.rs` のテスト
		// `create_sets_revision_to_one` と同じ前提）。
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
		groupId = group.id;

		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: TAG_NAME,
				collectionGroupId: group.id,
				// modbus-tcp 接続配下なので Modbus 参照番号形式が必要
				// （他 tags spec と同じ理由）。
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
		const tag = (await tagRes.json()) as TagResponse;
		tagId = tag.id;
		expect(tag.revision).toBe(1);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('別経路が先に更新（revision が進む） → UI が古い revision で保存すると 409 になり、黙って上書きしない', async () => {
		await page.goto('/tags');
		await page.getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		// UI の編集フォームはここで revision=1 を掴んだ状態。この後、UI を
		// 経由しない別クライアント相当（`page.request.put`）が
		// `expectedRevision: 1` で先に更新を確定させ、サーバー側の
		// revision を 2 に進める。
		const externalUpdateRes = await page.request.put(`/api/tags/${tagId}`, {
			headers: authedHeaders,
			data: {
				name: TAG_NAME,
				collectionGroupId: groupId,
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				unit: EXTERNAL_UNIT,
				enabled: true,
				writable: false,
				tagKind: 'plc',
				expectedRevision: 1
			}
		});
		expect(externalUpdateRes.ok()).toBe(true);
		const externalUpdated = (await externalUpdateRes.json()) as TagResponse;
		expect(externalUpdated.unit).toBe(EXTERNAL_UNIT);
		expect(externalUpdated.revision).toBe(2);

		// UI 側は自分が開いた時点の（今や古い）revision=1 を
		// expectedRevision として送るため、この保存はサーバー側で
		// revision 不一致（他クライアントが先に更新済み）と判定される。
		const unitInput = drawer.getByLabel('単位');
		await unitInput.fill(UI_ATTEMPTED_UNIT);
		await drawer.getByRole('button', { name: '保存' }).click();

		// 受け入れ条件「他セッション更新を黙って上書きしない」: 成功
		// トースト「更新しました」は出ない。
		await expect(page.getByText('更新しました')).toHaveCount(0);
		// 代わりに競合の通知（`RegistryMutationError::TagRevisionConflict`
		// の `message` をそのままトーストに表示したもの）が出る。
		await expect(
			page.getByText('他のクライアントがこのタグを更新済みです。再読込してから保存してください。')
		).toBeVisible();

		// フォームはローカルの未保存編集（UI_ATTEMPTED_UNIT）を破棄し、
		// サーバー最新値（外部更新の EXTERNAL_UNIT）で上書きされている -
		// 差分の並列表示は本 PR のスコープ外だが、少なくとも UI 側の値が
		// 黙って勝つことはない。
		await expect(unitInput).toHaveValue(EXTERNAL_UNIT);

		// サーバー側も外部更新の値のまま（UI の保存試行では変わっていない）。
		const afterRes = await page.request.get(`/api/tags/${tagId}`, { headers: authedHeaders });
		expect(afterRes.ok()).toBe(true);
		const after = (await afterRes.json()) as TagResponse;
		expect(after.unit).toBe(EXTERNAL_UNIT);
		expect(after.revision).toBe(2);
	});

	test('サーバー最新の revision を expectedRevision に使えば、UI からの保存が成功する', async () => {
		// 前のテストでフォームは既にサーバー最新（revision=2）に更新済み
		// なので、そのまま何かを変更して保存すれば今度は成功する -
		// 「検出したら即詰み」ではなく「最新を取り直せば再試行できる」
		// ことの確認。
		const drawer = page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		const unitInput = drawer.getByLabel('単位');
		await unitInput.fill('after-reload-unit');
		await drawer.getByRole('button', { name: '保存' }).click();

		await expect(page.getByText('更新しました')).toBeVisible();

		const afterRes = await page.request.get(`/api/tags/${tagId}`, { headers: authedHeaders });
		expect(afterRes.ok()).toBe(true);
		const after = (await afterRes.json()) as TagResponse;
		expect(after.unit).toBe('after-reload-unit');
		expect(after.revision).toBe(3);
	});
});
