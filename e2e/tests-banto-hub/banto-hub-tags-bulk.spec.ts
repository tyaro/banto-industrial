/**
 * タグ一括操作（T18-3b、docs/banto-hub-t18-design.md「T18-3b 一括操作」、
 * TAG-UX-D 中「複数選択＋一括有効/無効・グループ移動、対象件数と差分を
 * 事前表示」）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-form.spec.ts`/`banto-hub-tags-revision.spec.ts` と同じ
 * パターン: 別 `describe.serial` ブロック（別 `page`）、認証・前提データは
 * `page.request` で直接 REST を叩いて作る（`simulation: true`、実 PLC 不要）。
 * 選択列は無く、`tag-selection-mode-toggle` で行クリックの意味を
 * 「編集を開く」⇔「選択を切り替える」で切替える方式（`+page.svelte` の
 * `selectionMode`/`toggleSelectRow` 参照）。共有 DB を壊さないよう固定名は
 * `RUN_ID` で一意化し、リトライ再走用の冪等掃除を持つ。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-bulk-plc-${RUN_ID}`;
const GROUP_A_NAME = `e2e-bulk-groupA-${RUN_ID}`;
const GROUP_B_NAME = `e2e-bulk-groupB-${RUN_ID}`;
const TAG_NAMES = [`e2e-bulk-tag1-${RUN_ID}`, `e2e-bulk-tag2-${RUN_ID}`, `e2e-bulk-tag3-${RUN_ID}`];
// 3タグに共通する検索用の接頭辞（一覧を対象3件へ絞る）。
const SEARCH_PREFIX = `e2e-bulk-tag`;

interface TagResponse {
	id: number;
	name: string;
	enabled: boolean;
	collectionGroupId: number;
}

async function cleanupExistingFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
		const targetGroups = groups.filter((g) => g.name === GROUP_A_NAME || g.name === GROUP_B_NAME);
		if (targetGroups.length > 0) {
			const groupIds = new Set(targetGroups.map((g) => g.id));
			const tagsRes = await request.get('/api/tags', { headers });
			if (tagsRes.ok()) {
				const tags = (await tagsRes.json()) as Array<{ id: number; collectionGroupId: number }>;
				for (const tag of tags.filter((t) => groupIds.has(t.collectionGroupId))) {
					await request.delete(`/api/tags/${tag.id}`, { headers });
				}
			}
			for (const g of targetGroups)
				await request.delete(`/api/collection-groups/${g.id}`, { headers });
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

test.describe.serial('banto-hub タグ一括操作 (T18-3b)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let groupAId: number;
	let groupBId: number;
	const tagIds: number[] = [];

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await cleanupExistingFixtures(page.request, authedHeaders);

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

		const groupARes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_A_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupARes.ok()).toBe(true);
		groupAId = ((await groupARes.json()) as { id: number }).id;

		const groupBRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_B_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupBRes.ok()).toBe(true);
		groupBId = ((await groupBRes.json()) as { id: number }).id;

		for (let i = 0; i < TAG_NAMES.length; i++) {
			const tagRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: {
					name: TAG_NAMES[i],
					collectionGroupId: groupAId,
					address: String(40001 + i),
					dataType: 'i16',
					decimals: 0,
					enabled: true,
					writable: false,
					tagKind: 'plc'
				}
			});
			expect(tagRes.ok()).toBe(true);
			tagIds.push(((await tagRes.json()) as { id: number }).id);
		}
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec が仮想化グリッドで自分の行を
		// 見失わないよう、作った接続/グループ/タグは実行後に片付ける）。
		await cleanupExistingFixtures(page.request, authedHeaders);
		await page.close();
	});

	/** 選択モード中に、検索で絞った3タグをすべて選択する（行クリック＝選択切替）。 */
	async function selectAllThreeTags(): Promise<void> {
		await page.goto('/tags');
		await page.getByPlaceholder('名前・アドレスで検索').fill(SEARCH_PREFIX);
		// 「複数選択」トグルが OFF のときだけ押す（前テストで ON のまま残る場合がある）。
		const toggle = page.getByTestId('tag-selection-mode-toggle');
		if ((await toggle.textContent())?.includes('複数選択を終了') !== true) {
			await toggle.click();
		}
		for (const name of TAG_NAMES) {
			await page.getByRole('gridcell', { name, exact: true }).click();
		}
		await expect(page.getByTestId('tag-bulk-bar')).toContainText('選択 3 件');
	}

	async function fetchTags(): Promise<TagResponse[]> {
		const res = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(res.ok()).toBe(true);
		return (await res.json()) as TagResponse[];
	}

	test('1. 一括で無効化: 対象件数を確認して適用すると enabled=false になる', async () => {
		await selectAllThreeTags();

		await page.getByTestId('tag-bulk-disable-open').click();
		const panel = page.getByTestId('tag-bulk-confirm-panel');
		await expect(panel).toBeVisible();
		await expect(panel).toContainText('対象 3 件');

		await page.getByTestId('tag-bulk-apply').click();
		// 成功後は確認パネルが閉じ、選択も解除される。
		await expect(panel).toBeHidden();

		const tags = await fetchTags();
		for (const id of tagIds) {
			expect(tags.find((t) => t.id === id)?.enabled).toBe(false);
		}
	});

	test('2. グループへ一括移動: 移動先を選んで適用すると collectionGroupId が変わる', async () => {
		// 前テストの適用で選択は解除済みなので、選択し直す。
		await selectAllThreeTags();

		await page.getByTestId('tag-bulk-move-open').click();
		const panel = page.getByTestId('tag-bulk-confirm-panel');
		await expect(panel).toBeVisible();
		await panel.getByTestId('tag-bulk-target-group').selectOption({ label: GROUP_B_NAME });
		await expect(panel).toContainText('対象 3 件');

		await page.getByTestId('tag-bulk-apply').click();
		await expect(panel).toBeHidden();

		const tags = await fetchTags();
		for (const id of tagIds) {
			expect(tags.find((t) => t.id === id)?.collectionGroupId).toBe(groupBId);
		}
	});

	test('3. 選択解除で一括操作バーが消える', async () => {
		await selectAllThreeTags();
		await page.getByTestId('tag-bulk-clear-selection').click();
		await expect(page.getByTestId('tag-bulk-bar')).toBeHidden();
	});
});
