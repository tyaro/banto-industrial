/**
 * T20 機能②a 構造体タグ登録（docs/banto-hub-t20-design.md §3.2）の実 DOM
 * 受け入れテスト。純関数の割付・衝突検出ロジック自体は
 * `structRegistration.test.ts`（vitest）で網羅済みのため、ここでは
 * 「構造体登録パネルを開き、複数フィールドを自動割付で登録すると一覧に
 * 反映される」「既存タグと重なる割付は登録できないよう拒否される」という
 * 実 DOM/実サーバー往復だけを確認する。
 *
 * `banto-hub-tags-continuous.spec.ts` と同じパターン: 別 `describe.serial`
 * ブロック（別 `page`）、認証・前提データ（PLC接続・収集グループ）は
 * `page.request` で直接 REST を叩いて作る。ファイル名は
 * `banto-hub-auth.ts` の注意書きどおり `banto-hub-smoke.spec.ts` より
 * 辞書順で後になっている。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-struct-plc';
const GROUP_NAME = 'e2e-struct-group';
const EXISTING_TAG_NAME = 'e2e-struct-existing';

interface TagResponse {
	id: number;
	name: string;
	collectionGroupId: number;
	address: string;
}

test.describe.serial('banto-hub 構造体タグ登録 DOM (T20 ②a)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let groupId: number;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ: シミュレーションモードの PLC接続 + その配下の収集
		// グループ（実 PLC/実ネットワークへは繋がない - `banto-hub-tags-
		// continuous.spec.ts` と同じ理由）。
		//
		// `banto-hub-tags-p0-2-preflight.spec.ts` が確認しているとおり、
		// SLMP 形式アドレス（`D100` 等）は modbus-tcp 接続配下では
		// preflight（全構成 dry-run）が拒否する - 構造体登録はSLMPデバイス
		// 記法の連続アドレス割付が前提の機能のため、接続は `slmp` にする
		// （`wordOrder` は省略してサーバー既定 `low_high` に任せる）。
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'slmp',
				host: '127.0.0.1',
				port: 5010,
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

		// 衝突検出テスト用に、既存タグを1件 D6001 へ先置きしておく。
		const existingTagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: EXISTING_TAG_NAME,
				collectionGroupId: group.id,
				address: 'D6001',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(existingTagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await page.close();
	});

	async function fetchTags(): Promise<TagResponse[]> {
		const res = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(res.ok()).toBe(true);
		return (await res.json()) as TagResponse[];
	}

	async function openStructDrawer(): Promise<void> {
		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByTestId('struct-reg-open').click();
		await expect(page.getByRole('dialog', { name: '構造体登録' })).toBeVisible();
	}

	test('1. 自動割付で2フィールドを登録すると、ワード数考慮の連続アドレスで一覧に反映される', async () => {
		await openStructDrawer();

		await page.getByTestId('struct-reg-base-address').fill('D5000');
		await page.getByTestId('struct-reg-field-name-0').fill('e2e-struct-a');
		// フィールド0の型は既定 i16（+1word）のまま。

		await page.getByTestId('struct-reg-field-add').click();
		await page.getByTestId('struct-reg-field-name-1').fill('e2e-struct-b');
		await page.getByTestId('struct-reg-field-type-1').selectOption('i32');

		// i16(1word) の次が i32(2word) の開始アドレスになるので D5000/D5001。
		await expect(page.getByTestId('struct-reg-preview-table')).toBeVisible();
		const rows = page.locator('[data-testid="struct-reg-preview-table"] tbody tr');
		await expect(rows).toHaveCount(2);
		await expect(rows.nth(0)).toContainText('e2e-struct-a');
		await expect(rows.nth(0)).toContainText('D5000');
		await expect(rows.nth(1)).toContainText('e2e-struct-b');
		await expect(rows.nth(1)).toContainText('D5001');

		// 衝突なしなので衝突パネルは出ない。
		await expect(page.getByTestId('struct-reg-collisions')).toHaveCount(0);

		await page.getByTestId('struct-reg-validate').click();
		await expect(page.getByText('構造体タグの検証OK: 2件登録できます')).toBeVisible();

		await page.getByTestId('struct-reg-apply').click();
		await expect(page.getByText('構造体タグを2件登録しました')).toBeVisible();

		// 設計「成功で一覧 reload()・パネルを閉じる」: Drawer は閉じる。
		await expect(page.getByRole('dialog', { name: '構造体登録' })).toHaveCount(0);

		const tags = await fetchTags();
		expect(
			tags.some(
				(t) => t.name === 'e2e-struct-a' && t.collectionGroupId === groupId && t.address === 'D5000'
			)
		).toBe(true);
		expect(
			tags.some(
				(t) => t.name === 'e2e-struct-b' && t.collectionGroupId === groupId && t.address === 'D5001'
			)
		).toBe(true);

		// 一覧（グリッド）にも反映されていること。
		await page.getByPlaceholder('名前・アドレスで検索').fill('e2e-struct-a');
		await expect(page.getByText('e2e-struct-a')).toBeVisible();
	});

	test('2. 既存タグとアドレスが重なる割付は衝突表示され、検証・登録ができない', async () => {
		await openStructDrawer();

		// D6000(i16, 1word) と D6001(i32, 2word) の2フィールド -> 2フィールド目が
		// D6001-D6002 を占有し、既存タグ e2e-struct-existing（D6001）と重なる。
		await page.getByTestId('struct-reg-base-address').fill('D6000');
		await page.getByTestId('struct-reg-field-name-0').fill('e2e-struct-collide-a');
		await page.getByTestId('struct-reg-field-add').click();
		await page.getByTestId('struct-reg-field-name-1').fill('e2e-struct-collide-b');
		await page.getByTestId('struct-reg-field-type-1').selectOption('i32');

		await expect(page.getByTestId('struct-reg-collisions')).toBeVisible();
		await expect(page.getByTestId('struct-reg-collisions')).toContainText(EXISTING_TAG_NAME);

		await expect(page.getByTestId('struct-reg-validate')).toBeDisabled();
		await expect(page.getByTestId('struct-reg-apply')).toBeDisabled();

		const tagsBefore = await fetchTags();
		expect(tagsBefore.some((t) => t.name === 'e2e-struct-collide-a')).toBe(false);
	});
});
