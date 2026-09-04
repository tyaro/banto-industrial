/**
 * T20 機能②b オフセットコピー（docs/banto-hub-t20-design.md §3.2、2026-09-05
 * オーナー決定「命名ルール」）の実 DOM 受け入れテスト。
 *
 * 純関数の割付・命名・衝突検出ロジック自体は `offsetCopy.test.ts`（vitest）
 * で網羅済みのため、ここでは「タグ一覧で複数選択 → 一括操作バーの
 * 『オフセットコピー』→ プレビュー → 検証(dry-run) → 登録」という実 DOM/実
 * サーバー往復と、命名ルール（デバイス名由来／意味名の両方）が実際に反映
 * されることだけを確認する。
 *
 * `banto-hub-tags-struct.spec.ts`/`banto-hub-tags-bulk.spec.ts` と同じ
 * パターン: 別 `describe.serial` ブロック（別 `page`）、認証・前提データは
 * `page.request` で直接 REST を叩いて作る。接続は構造体登録②aと同じ理由
 * （SLMP デバイス記法のアドレス算術が前提）で `slmp` にする。ファイル名は
 * `banto-hub-auth.ts` の注意書きどおり `banto-hub-smoke.spec.ts` より
 * 辞書順で後になっている。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-offsetcopy-plc';
const GROUP_NAME = 'e2e-offsetcopy-group';
/** デバイス名由来タグ（名前がそのアドレスのデバイス表記そのもの）。 */
const DEVICE_NAME_TAG = 'D3000';
/** 意味のある構造体名タグ（末尾に数字を付ける命名ルールの対象）。 */
const MEANING_NAME_TAG = 'temp01';

interface TagResponse {
	id: number;
	name: string;
	collectionGroupId: number;
	address: string;
}

test.describe.serial('banto-hub オフセットコピー DOM (T20 ②b)', () => {
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
		// struct.spec.ts` と同じ理由）。
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'slmp',
				host: '127.0.0.1',
				port: 5011,
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

		// デバイス名由来タグ（名前=アドレスのデバイス表記）と、意味のある
		// 構造体名タグの2件を、隣接するアドレス（D3000/D3001）に用意する。
		const deviceTagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: DEVICE_NAME_TAG,
				collectionGroupId: group.id,
				address: 'D3000',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(deviceTagRes.ok()).toBe(true);

		const meaningTagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: MEANING_NAME_TAG,
				collectionGroupId: group.id,
				address: 'D3001',
				dataType: 'i16',
				decimals: 1,
				unit: '℃',
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(meaningTagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await page.close();
	});

	async function fetchTags(): Promise<TagResponse[]> {
		const res = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(res.ok()).toBe(true);
		return (await res.json()) as TagResponse[];
	}

	test('デバイス名由来タグ＋意味名タグを選択してオフセットコピーすると、命名ルールどおりのコピー先が登録される', async () => {
		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();

		// 選択モードへ切替（`banto-hub-tags-bulk.spec.ts` と同じ作法）。
		const toggle = page.getByTestId('tag-selection-mode-toggle');
		if ((await toggle.textContent())?.includes('複数選択を終了') !== true) {
			await toggle.click();
		}

		// デバイス名由来タグは名前=アドレス（"D3000"）のため、名前列・アドレス列
		// 両方が同じテキストの gridcell にマッチする - `.first()` で名前列
		// （列定義の並び順が id/name/収集グループ/address... のため name が先）
		// を選ぶ（クリックの意味は「行の選択切り替え」なのでどちらの列でも
		// 実際の効果は同じだが、strict mode violation を避けるため一意化する）。
		await page.getByRole('gridcell', { name: DEVICE_NAME_TAG, exact: true }).first().click();
		await page.getByRole('gridcell', { name: MEANING_NAME_TAG, exact: true }).click();
		await expect(page.getByTestId('tag-bulk-bar')).toContainText('選択 2 件');

		await page.getByTestId('tag-bulk-offset-copy-open').click();
		const panel = page.getByTestId('tag-bulk-offset-copy-panel');
		await expect(panel).toBeVisible();

		await page.getByTestId('tag-bulk-offset-copy-words').fill('100');

		// D3000(デバイス名由来) -> D3100/D3100、D3001(temp01) -> D3101/temp02。
		const rows = page.locator('[data-testid="tag-bulk-offset-copy-preview-table"] tbody tr');
		await expect(rows).toHaveCount(2);
		await expect(rows.nth(0)).toContainText(DEVICE_NAME_TAG);
		await expect(rows.nth(0)).toContainText('D3100');
		await expect(rows.nth(1)).toContainText(MEANING_NAME_TAG);
		await expect(rows.nth(1)).toContainText('temp02');
		await expect(rows.nth(1)).toContainText('D3101');

		// 衝突なしなのでエラー一覧は出ない。
		await expect(page.getByTestId('tag-bulk-offset-copy-errors')).toHaveCount(0);

		await page.getByTestId('tag-bulk-offset-copy-validate').click();
		await expect(page.getByText('オフセットコピーの検証OK: 2件登録できます')).toBeVisible();

		await page.getByTestId('tag-bulk-offset-copy-apply').click();
		await expect(page.getByText('オフセットコピーで2件のタグを複製しました')).toBeVisible();

		// 成功でパネルが閉じ、選択も解除される。
		await expect(panel).toBeHidden();

		const tags = await fetchTags();
		expect(
			tags.some(
				(t) => t.name === 'D3100' && t.collectionGroupId === groupId && t.address === 'D3100'
			)
		).toBe(true);
		expect(
			tags.some(
				(t) => t.name === 'temp02' && t.collectionGroupId === groupId && t.address === 'D3101'
			)
		).toBe(true);

		// 一覧（グリッド）にも反映されていること（D3100 も名前=アドレスなので `.first()`）。
		await expect(page.getByRole('gridcell', { name: 'D3100', exact: true }).first()).toBeVisible();
		await expect(page.getByRole('gridcell', { name: 'temp02', exact: true })).toBeVisible();
	});

	test('既存タグとアドレスが重なるオフセットは衝突表示され、検証・実行ができない', async () => {
		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();

		const toggle = page.getByTestId('tag-selection-mode-toggle');
		if ((await toggle.textContent())?.includes('複数選択を終了') !== true) {
			await toggle.click();
		}

		// 前テストで D3100/D3101 が既に存在するため、D3000/D3001 を再度
		// +100 でコピーしようとすると D3100/D3101 と衝突する。
		// デバイス名由来タグは名前=アドレス（"D3000"）のため、名前列・アドレス列
		// 両方が同じテキストの gridcell にマッチする - `.first()` で名前列
		// （列定義の並び順が id/name/収集グループ/address... のため name が先）
		// を選ぶ（クリックの意味は「行の選択切り替え」なのでどちらの列でも
		// 実際の効果は同じだが、strict mode violation を避けるため一意化する）。
		await page.getByRole('gridcell', { name: DEVICE_NAME_TAG, exact: true }).first().click();
		await page.getByRole('gridcell', { name: MEANING_NAME_TAG, exact: true }).click();
		await expect(page.getByTestId('tag-bulk-bar')).toContainText('選択 2 件');

		await page.getByTestId('tag-bulk-offset-copy-open').click();
		await page.getByTestId('tag-bulk-offset-copy-words').fill('100');

		await expect(page.getByTestId('tag-bulk-offset-copy-errors')).toBeVisible();
		await expect(page.getByTestId('tag-bulk-offset-copy-validate')).toBeDisabled();
		await expect(page.getByTestId('tag-bulk-offset-copy-apply')).toBeDisabled();

		const tagsBefore = await fetchTags();
		const before3100 = tagsBefore.filter((t) => t.name === 'D3100').length;
		expect(before3100).toBe(1);
	});
});
