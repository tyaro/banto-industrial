/**
 * タグ更新の楽観的ロック + 競合時の差分表示 UI（T18-1、TAG-UX-C 4点目、
 * docs/banto-hub-desktop-plan.md §9.4「revision / ETag で後勝ち上書きを
 * 防ぎ、競合時は差分を表示する」）の実 DOM 受け入れテスト。検出（revision
 * + HTTP 409）は `cursor/t18-1-tags-revision-e3cb` 済み。本 spec は続く
 * `cursor/t18-1-tags-conflict-diff-e3cb` で追加した差分表示 UI（フィールド
 * 単位の並列比較・「サーバー最新を採用」/「自分の内容で再保存」）を確認する
 * - **旧版はここで「フォームが即サーバー最新値に上書きされる」ことを
 * 確認していたが、挙動変更（ローカル編集を破棄せずパネルで選ばせる）に
 * 伴い、その部分のアサーションは書き換えている**。
 *
 * `banto-hub-tags-busy.spec.ts`/`banto-hub-tags-form.spec.ts` と同じ
 * パターン: 別 `describe.serial` ブロック（別 `page`）、認証・前提データ
 * （PLC接続・収集グループ・タグ）は `page.request` で直接 REST を叩いて
 * 作る。競合は「別経路（`page.request.put`、UI を経由しない別クライアント
 * 相当）が UI より先に同じタグを更新して revision を進める」ことで作る -
 * UI 側の編集フォームは古い revision を掴んだまま保存を試みる。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-revision-plc';
const GROUP_NAME = 'e2e-revision-group';
const TAG_NAME = 'e2e-revision-tag';

/** 1回目の競合: 他クライアント（UI を経由しない別経路）が確定させる単位。 */
const EXTERNAL_UNIT = 'external-kPa';
/** 1回目の競合: UI 側がフォームで入力しようとする単位。 */
const UI_ATTEMPTED_UNIT = 'ui-should-not-win';
/** サーバー最新採用後、そのまま保存し直す単位（3件目のテスト）。 */
const AFTER_RESOLVE_UNIT = 'after-resolve-unit';
/** 2回目の競合: 他クライアントが確定させる単位。 */
const EXTERNAL_UNIT_2 = 'external-kPa-2';
/** 2回目の競合: UI 側が「自分の内容で再保存」で勝たせる単位。 */
const UI_ATTEMPTED_UNIT_2 = 'ui-should-win-this-time';

interface TagResponse {
	id: number;
	unit: string | null;
	revision: number;
}

/**
 * この spec が使う固定名（CONNECTION_NAME/GROUP_NAME）の PLC接続・収集
 * グループ・配下タグを、存在すれば掃除する。plc_connections / collection_groups
 * は `name` に UNIQUE 制約があるため（`crates/banto-tags/migrations/0001,
 * 0002`）、失敗テストが `describe.serial` でリトライされて beforeAll が
 * 再走すると、前回作成済みの同名リソースが残ったまま POST して UNIQUE
 * 違反で not-ok になり beforeAll ごと落ちる - それを防ぐための冪等掃除。
 * FK は RESTRICT なので削除順は タグ → グループ → 接続。
 */
async function cleanupExistingFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
		const existingGroup = groups.find((g) => g.name === GROUP_NAME);
		if (existingGroup) {
			const tagsRes = await request.get('/api/tags', { headers });
			if (tagsRes.ok()) {
				const tags = (await tagsRes.json()) as Array<{ id: number; collectionGroupId: number }>;
				for (const tag of tags.filter((t) => t.collectionGroupId === existingGroup.id)) {
					await request.delete(`/api/tags/${tag.id}`, { headers });
				}
			}
			await request.delete(`/api/collection-groups/${existingGroup.id}`, { headers });
		}
	}

	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as Array<{ id: number; name: string }>;
		const existingConnection = connections.find((c) => c.name === CONNECTION_NAME);
		if (existingConnection) {
			await request.delete(`/api/plc-connections/${existingConnection.id}`, { headers });
		}
	}
}

test.describe.serial('banto-hub タグ更新の楽観的ロック + 競合時の差分表示 (TAG-UX-C)', () => {
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

		// 失敗テストのリトライで beforeAll が再走した場合に備え、前回分の
		// 同名リソースを先に掃除しておく（初回実行では何もしない）。
		await cleanupExistingFixtures(page.request, authedHeaders);

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

	test('別経路が先に更新（revision が進む） → UI が古い revision で保存すると 409 になり、差分パネルが表示される', async () => {
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

		// 差分表示 UI（本 spec の本題）: フォーム上部に競合パネルが出て、
		// 「あなたの入力」列に UI 側の入力、「サーバー最新」列に外部更新の
		// 値が並ぶ。
		await expect(drawer.getByText('他のクライアントが先に更新しています')).toBeVisible();
		const conflictRow = drawer.locator('tr', { hasText: '単位' });
		await expect(conflictRow).toContainText(UI_ATTEMPTED_UNIT);
		await expect(conflictRow).toContainText(EXTERNAL_UNIT);

		// 挙動変更点: ローカルの未保存編集（UI_ATTEMPTED_UNIT）は破棄され
		// ない - フォーム自体は依然としてユーザーが入力した値を保持する
		// （黙ってサーバー値に上書きされるわけでも、黙ってローカル値が
		// 勝つわけでもなく、ユーザーが選ぶまで両方が見える）。
		await expect(unitInput).toHaveValue(UI_ATTEMPTED_UNIT);

		// サーバー側も外部更新の値のまま（UI の保存試行では変わっていない）。
		const afterRes = await page.request.get(`/api/tags/${tagId}`, { headers: authedHeaders });
		expect(afterRes.ok()).toBe(true);
		const after = (await afterRes.json()) as TagResponse;
		expect(after.unit).toBe(EXTERNAL_UNIT);
		expect(after.revision).toBe(2);
	});

	test('「サーバー最新を採用」を押すとフォームがサーバー値になり、差分パネルが消える', async () => {
		const drawer = page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();
		await expect(drawer.getByText('他のクライアントが先に更新しています')).toBeVisible();

		await drawer.getByRole('button', { name: 'サーバー最新を採用' }).click();

		await expect(drawer.getByText('他のクライアントが先に更新しています')).toHaveCount(0);
		await expect(drawer.getByLabel('単位')).toHaveValue(EXTERNAL_UNIT);
	});

	test('サーバー最新を採用した後にそのまま保存すると、通常どおり成功する', async () => {
		// 前のテストでフォームは既にサーバー最新（revision=2）に更新済み
		// なので、そのまま何かを変更して保存すれば今度は成功する -
		// 「検出したら即詰み」ではなく「最新を取り直せば再試行できる」
		// ことの確認。
		const drawer = page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		const unitInput = drawer.getByLabel('単位');
		await unitInput.fill(AFTER_RESOLVE_UNIT);
		await drawer.getByRole('button', { name: '保存' }).click();

		// 同一 page で複数回保存するため、先行テストのトーストが
		// AUTO_DISMISS_MS（4000ms）以内に消えないと `getByText` が複数
		// 要素にマッチし strict mode violation になる - 最新トーストに
		// スコープする。
		await expect(page.getByText('更新しました').last()).toBeVisible();

		const afterRes = await page.request.get(`/api/tags/${tagId}`, { headers: authedHeaders });
		expect(afterRes.ok()).toBe(true);
		const after = (await afterRes.json()) as TagResponse;
		expect(after.unit).toBe(AFTER_RESOLVE_UNIT);
		expect(after.revision).toBe(3);
	});

	test('2度目の競合で「自分の内容で再保存」を押すと、ローカルの入力が勝って revision が進む', async () => {
		const drawer = page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		// 再び別経路が先に更新して revision を 3 → 4 に進める。
		const externalUpdateRes = await page.request.put(`/api/tags/${tagId}`, {
			headers: authedHeaders,
			data: {
				name: TAG_NAME,
				collectionGroupId: groupId,
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				unit: EXTERNAL_UNIT_2,
				enabled: true,
				writable: false,
				tagKind: 'plc',
				expectedRevision: 3
			}
		});
		expect(externalUpdateRes.ok()).toBe(true);
		const externalUpdated = (await externalUpdateRes.json()) as TagResponse;
		expect(externalUpdated.revision).toBe(4);

		const unitInput = drawer.getByLabel('単位');
		await unitInput.fill(UI_ATTEMPTED_UNIT_2);
		await drawer.getByRole('button', { name: '保存' }).click();

		// 再び競合パネルが出て、今回はローカル/サーバーそれぞれの新しい値が
		// 表に並ぶ。
		await expect(drawer.getByText('他のクライアントが先に更新しています')).toBeVisible();
		const conflictRow = drawer.locator('tr', { hasText: '単位' });
		await expect(conflictRow).toContainText(UI_ATTEMPTED_UNIT_2);
		await expect(conflictRow).toContainText(EXTERNAL_UNIT_2);

		await drawer.getByRole('button', { name: '自分の内容で再保存' }).click();

		// 「自分の内容で再保存」は `expectedRevision` をサーバー最新
		// （競合検出時に更新済みの revision=4）に差し替えて再送信するため、
		// 今度は成功し、ローカルの入力（UI_ATTEMPTED_UNIT_2）が勝つ。
		// 同一 page で複数回保存するため最新トーストにスコープ（strict mode
		// violation 回避）。
		await expect(page.getByText('更新しました').last()).toBeVisible();
		await expect(drawer.getByText('他のクライアントが先に更新しています')).toHaveCount(0);
		await expect(unitInput).toHaveValue(UI_ATTEMPTED_UNIT_2);

		const afterRes = await page.request.get(`/api/tags/${tagId}`, { headers: authedHeaders });
		expect(afterRes.ok()).toBe(true);
		const after = (await afterRes.json()) as TagResponse;
		expect(after.unit).toBe(UI_ATTEMPTED_UNIT_2);
		expect(after.revision).toBe(5);
	});
});
