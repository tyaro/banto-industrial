/**
 * 「表示グループ」ページ（`/groups`）の右ペイン「タグ選択」から、**未登録の Hub のタグ**を
 * ペンに追加する E2E 固定（#1179 PR6b）。
 *
 * このリポジトリの E2E には Hub を実際に立てる仕組みが無いので、`user-tags-from-hub.spec.ts` と
 * 同じく **Hub のカタログだけ**を `page.route` で差し替える（`GET /api/hub/catalog` は実サーバーへ通して
 * `registered` は実サーバーのレジストリのまま使い、`status`・`tags` を固定のカタログにする）。
 * **保存（`POST /api/display-groups`）は実サーバー**なので、`hubTag` のペンが記録項目に登録され、
 * Hub 接続・収集グループも作られる本物の経路を通る。
 *
 * 固定すること:
 * 1. タグ選択の検索で、未登録の Hub タグには「未登録・保存で記録に追加」の印が付き、
 *    登録できない型（文字列）は理由つきで選べない。
 * 2. 未登録の Hub タグを追加するとペンが「未登録（保存で記録に追加）」で並び、登録先
 *    （既定は新しいグループ・名前は「Hub のタグ」）が「ペン」タブに出る。
 * 3. 登録先の名前を入れて作成すると、記録項目・収集グループが作られ、ペンは記録項目に割り当たる
 *    （再読み込みしても、ペンのタグは「未登録」ではなく記録項目）。
 * 4. 開き直したタグ選択では、そのタグは「追加済み」で選べない。
 *
 * ファイル名: `user-display-groups.spec.ts` の直後（管理者アカウントは `smoke.spec.ts` が作る）。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import {
	addGroupButton,
	addTagBySearch,
	center as centerPane,
	closeTagPicker,
	groupButton,
	openPensTab,
	openTagPicker,
	pickerRow,
	tagPicker
} from './display-groups-page';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
type ApiHeaders = Record<string, string>;

const GROUP_NAME = 'E2E-DGHUB-ライン';
const HUB_GROUP_NAME = 'E2E-DGHUB-収集';
const TEMP = 'E2EDGHUB.Fast.temp';
const LABEL = 'E2EDGHUB.Fast.label';

const CATALOG_TAGS = [
	{
		externalName: TEMP,
		connection: 'E2EDGHUB',
		group: 'Fast',
		name: 'temp',
		dataType: 'f32',
		unit: 'degC',
		decimals: 1,
		tagKind: 'plc'
	},
	{
		externalName: LABEL,
		connection: 'E2EDGHUB',
		group: 'Fast',
		name: 'label',
		dataType: 'string',
		unit: null,
		decimals: 0,
		tagKind: 'plc'
	}
];

interface Row {
	id: number;
	name: string;
	protocol?: string;
	address?: string;
}

async function fetchApiHeaders(request: APIRequestContext): Promise<ApiHeaders> {
	const res = await request.post('/api/auth/login', {
		headers: CSRF_HEADERS,
		data: { username: ADMIN_USERNAME, password: ADMIN_PASSWORD }
	});
	const body = (await res.json()) as { success: boolean; token?: string; error?: string };
	if (!body.success || !body.token) {
		throw new Error(`REST のログインに失敗しました: ${body.error ?? res.status()}`);
	}
	return { ...CSRF_HEADERS, Authorization: `Bearer ${body.token}` };
}

async function listRows(
	request: APIRequestContext,
	headers: ApiHeaders,
	url: string
): Promise<Row[]> {
	const res = await request.get(url, { headers });
	if (!res.ok()) throw new Error(`GET ${url} が ${res.status()}`);
	return (await res.json()) as Row[];
}

/** このスペックが作ったもの（表示グループ → タグ → 収集グループ → Hub 接続）を消す。 */
async function cleanupFixtures(request: APIRequestContext, headers: ApiHeaders): Promise<void> {
	const failures: string[] = [];
	const remove = async (url: string): Promise<void> => {
		const res = await request.delete(url, { headers });
		if (res.status() !== 204 && res.status() !== 404) {
			failures.push(`DELETE ${url} が ${res.status()}: ${await res.text()}`);
		}
	};
	try {
		for (const group of (await listRows(request, headers, '/api/display-groups')).filter(
			(g) => g.name === GROUP_NAME
		)) {
			await remove(`/api/display-groups/${group.id}`);
		}
		for (const tag of (await listRows(request, headers, '/api/tags')).filter((t) =>
			t.address?.startsWith('E2EDGHUB.')
		)) {
			await remove(`/api/tags/${tag.id}`);
		}
		for (const group of (await listRows(request, headers, '/api/collection-groups')).filter(
			(g) => g.name === HUB_GROUP_NAME
		)) {
			await remove(`/api/collection-groups/${group.id}`);
		}
		for (const conn of (await listRows(request, headers, '/api/plc-connections')).filter(
			(c) => c.protocol === 'hub'
		)) {
			await remove(`/api/plc-connections/${conn.id}`);
		}
	} catch (err) {
		failures.push(err instanceof Error ? err.message : String(err));
	}
	if (failures.length > 0) throw new Error(`掃除に失敗しました:\n${failures.join('\n')}`);
}

/** 実サーバーの catalog の `registered` を保ったまま、カタログだけを差し替える。 */
async function routeCatalog(page: Page): Promise<void> {
	await page.route('**/api/hub/catalog', async (route) => {
		const real = (await (await route.fetch()).json()) as { registered: string[] };
		await route.fulfill({
			json: {
				status: { state: 'connected', tagCount: CATALOG_TAGS.length },
				tags: CATALOG_TAGS,
				registered: real.registered,
				legacySelection: []
			}
		});
	});
}

test.describe.serial('chronogazer 「表示グループ」の Hub タグ追加（#1179 PR6b）', () => {
	let page: Page;
	let headers: ApiHeaders;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.addInitScript(() => localStorage.setItem('banto.navLayout', 'sidebar'));
		headers = await fetchApiHeaders(page.request);
		await cleanupFixtures(page.request, headers);
		await routeCatalog(page);

		await page.goto('/login');
		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		await expect(page).toHaveURL(/\/monitor$/);
	});

	test.afterAll(async () => {
		try {
			await cleanupFixtures(page.request, headers);
		} finally {
			await page.close();
		}
	});

	const center = () => centerPane(page);

	test('1. 検索で未登録の印が付き、登録できない型は理由つきで選べない', async () => {
		await page
			.locator('aside nav')
			.getByRole('link', { name: /表示グループ/ })
			.click();
		await expect(page).toHaveURL(/\/groups$/);
		await addGroupButton(page).click();
		await center().getByLabel('名前').fill(GROUP_NAME);
		await openPensTab(page);

		const picker = await openTagPicker(page);
		await expect(picker.getByTestId('pen-tag-picker-title')).toContainText(
			'追加先: 新しい表示グループ'
		);
		await picker.getByTestId('pen-tag-picker-search').fill('E2EDGHUB');
		const temp = pickerRow(page, 'temp');
		await expect(temp).toContainText('未登録・保存で記録に追加');
		await expect(temp.getByRole('button', { name: '「temp」を追加' })).toBeEnabled();
		const label = pickerRow(page, 'label');
		await expect(label).toContainText('string 型は記録できません');
		await expect(label.getByRole('button', { name: '「label」を追加' })).toBeDisabled();
	});

	test('2. 追加すると未登録のペンが並び、登録先（新しいグループ）が出る', async () => {
		await addTagBySearch(page, 'temp');
		await expect(pickerRow(page, 'temp')).toContainText('追加済み');
		await closeTagPicker(page);

		await expect(center().getByTestId('display-group-pen-1')).toContainText(
			'未登録（保存で記録に追加）'
		);
		const target = center().getByTestId('hub-target');
		await expect(target.getByLabel('名前')).toHaveValue('Hub のタグ');
		await target.getByLabel('名前').fill(HUB_GROUP_NAME);
		await expect(center().locator('.banto-unsaved')).toHaveText('未保存の変更があります');
	});

	test('3. 作成すると記録項目に登録され、ペンはその記録項目に割り当たる', async () => {
		await center().getByRole('button', { name: '作成', exact: true }).click();
		await expect(groupButton(page, GROUP_NAME)).toContainText('ペン 1 本');
		await expect(center().locator('.banto-unsaved')).toHaveCount(0);

		const tags = await listRows(page.request, headers, '/api/tags');
		expect(tags.some((t) => t.address === TEMP)).toBe(true);
		const groups = await listRows(page.request, headers, '/api/collection-groups');
		expect(groups.some((g) => g.name === HUB_GROUP_NAME)).toBe(true);

		await page.reload();
		await groupButton(page, GROUP_NAME).click();
		await openPensTab(page);
		const pen = center().getByTestId('display-group-pen-1');
		await expect(pen).toContainText('temp');
		await expect(pen).not.toContainText('未登録');
		await expect(center().getByTestId('hub-target')).toHaveCount(0);
	});

	test('4. 開き直したタグ選択では、そのタグは記録中・追加済みで選べない', async () => {
		const picker = await openTagPicker(page);
		await picker.getByTestId('pen-tag-picker-search').fill('E2EDGHUB');
		const temp = pickerRow(page, 'temp');
		await expect(temp).toContainText('追加済み');
		await expect(temp.getByRole('button', { name: '「temp」を追加' })).toBeDisabled();
		await expect(tagPicker(page).getByTestId('pen-tag-picker-remaining')).toContainText(
			'あと 15 本'
		);
	});
});
