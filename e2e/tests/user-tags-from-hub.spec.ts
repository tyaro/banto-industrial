/**
 * 「Hub から追加」（#383 段階3 P4、2026-10-11 オーナー決定）の実 DOM 固定。
 *
 * このリポジトリの E2E には Hub を実際に立てる仕組みが無い（`user-settings-hub
 * .spec.ts` の doc comment）。そこで **Hub のカタログだけ**を `page.route` で
 * 差し替える: `GET /api/hub/catalog` は実サーバーへ通して（`route.fetch()`）
 * **登録済みの外部名（`registered`）は実サーバーのレジストリのまま**使い、
 * `status`・`tags`・`legacySelection` だけを固定のカタログに置き換える。
 * **登録（`POST /api/tags/from-hub`）は実サーバー**なので、Hub 接続・収集グループ・
 * タグの作成と、名前の衝突の行ごとのエラーは本物の経路を通る。
 *
 * 固定すること:
 * 1. 以前の Hub 接続の画面で選んでいたタグが、選べるものだけ事前にチェック
 *    され、注記が出る。文字列型は理由付きで選べない。
 * 2. 新しいグループを作って登録すると、Hub 接続（読み取り専用の行）・グループ・
 *    タグができ、トーストが出てダイアログが閉じる。
 * 3. 開き直すと、登録済みのタグは「登録済み」で選べない（実サーバーの
 *    `registered`）。同じ名前のタグを同じグループに登録しようとすると、その
 *    行に理由が出て何も登録されない（黙って改名しない）。
 * 4. 層の約束: 未保存の選択がある間は Esc で閉じず、× は確認を出す。
 * 5. Hub 経由のタグの編集フォームにスケーリングの欄が無い。Hub 接続の行は
 *    読み取り専用（ホストの欄が無い）。
 *
 * フィクスチャは `beforeAll` の先頭と `afterAll` の両方で REST から掃除する
 * （チェックリスト §3。タグ → 収集グループ → Hub 接続の順）。
 *
 * ファイル名: `smoke.spec.ts`（初回セットアップ）より後でなければならない
 * （辞書順が実行順）。`user-tags-*` は `smoke` より後。
 */
import { expect, test, type APIRequestContext, type Dialog, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
type ApiHeaders = Record<string, string>;

const GROUP_NAME = 'E2E-HUB-グループ';
const TEMP = 'E2EHUB.Fast.temp';
const RUN = 'E2EHUB.Fast.run';
const LABEL = 'E2EHUB.Fast.label';
/** 名前が `temp` で衝突する、別の Hub グループのタグ。 */
const OTHER_TEMP = 'E2EHUB.Slow.temp';

const CATALOG_TAGS = [
	{ externalName: TEMP, name: 'temp', dataType: 'f32', unit: 'degC', decimals: 1, tagKind: 'plc' },
	{ externalName: RUN, name: 'run', dataType: 'bit', unit: null, decimals: 0, tagKind: 'plc' },
	{
		externalName: LABEL,
		name: 'label',
		dataType: 'string',
		unit: null,
		decimals: 0,
		tagKind: 'plc'
	},
	{
		externalName: OTHER_TEMP,
		name: 'temp',
		dataType: 'f32',
		unit: 'degC',
		decimals: 1,
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

/** このスペックが作ったもの（外部名 `E2EHUB.`・グループ名・Hub 接続）を消す。 */
async function cleanupFixtures(request: APIRequestContext, headers: ApiHeaders): Promise<void> {
	const failures: string[] = [];
	const list = async (url: string): Promise<Row[]> => {
		const res = await request.get(url, { headers });
		if (!res.ok()) throw new Error(`GET ${url} が ${res.status()}`);
		return (await res.json()) as Row[];
	};
	const remove = async (url: string): Promise<void> => {
		const res = await request.delete(url, { headers });
		if (res.status() !== 204 && res.status() !== 404) {
			failures.push(`DELETE ${url} が ${res.status()}: ${await res.text()}`);
		}
	};
	try {
		for (const tag of (await list('/api/tags')).filter((t) => t.address?.startsWith('E2EHUB.'))) {
			await remove(`/api/tags/${tag.id}`);
		}
		for (const group of (await list('/api/collection-groups')).filter(
			(g) => g.name === GROUP_NAME
		)) {
			await remove(`/api/collection-groups/${group.id}`);
		}
		for (const conn of (await list('/api/plc-connections')).filter((c) => c.protocol === 'hub')) {
			await remove(`/api/plc-connections/${conn.id}`);
		}
	} catch (err) {
		failures.push(err instanceof Error ? err.message : String(err));
	}
	if (failures.length > 0) throw new Error(`掃除に失敗しました:\n${failures.join('\n')}`);
}

/** 実サーバーの catalog の `registered` を保ったまま、カタログだけを差し替える。 */
async function routeCatalog(page: Page, legacySelection: string[]): Promise<void> {
	await page.route('**/api/hub/catalog', async (route) => {
		const real = (await (await route.fetch()).json()) as { registered: string[] };
		await route.fulfill({
			json: {
				status: { state: 'connected', tagCount: CATALOG_TAGS.length },
				tags: CATALOG_TAGS,
				registered: real.registered,
				legacySelection
			}
		});
	});
}

test.describe.serial('chronogazer タグ設定の「Hub から追加」（#383 段階3 P4）', () => {
	let page: Page;
	let headers: ApiHeaders;
	let confirmAnswer: 'accept' | 'dismiss' = 'accept';
	const confirms: string[] = [];

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		page.on('dialog', (dialog: Dialog) => {
			confirms.push(dialog.message());
			void (confirmAnswer === 'accept' ? dialog.accept() : dialog.dismiss());
		});
		headers = await fetchApiHeaders(page.request);
		await cleanupFixtures(page.request, headers);

		await page.goto('/login');
		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		await expect(page).toHaveURL(/\/monitor$/);
	});

	test.afterAll(async () => {
		try {
			await page.unroute('**/api/hub/catalog');
			await cleanupFixtures(page.request, headers);
		} finally {
			await page.close();
		}
	});

	const dialog = () => page.getByRole('dialog', { name: 'Hub から追加' });
	const checkbox = (externalName: string) =>
		dialog().locator('li').filter({ hasText: externalName }).getByRole('checkbox');

	test('1. 以前の選択は選べるものだけ事前にチェックされ、文字列型は理由付きで選べない', async () => {
		await routeCatalog(page, [TEMP, LABEL]);
		await page.goto('/tags');
		await page.getByRole('button', { name: 'Hub から追加' }).click();
		await expect(dialog()).toBeVisible();

		await expect(dialog().getByText('以前の Hub 接続の画面で選んでいたタグを')).toBeVisible();
		await expect(checkbox(TEMP)).toBeChecked();
		await expect(checkbox(LABEL)).toBeDisabled();
		await expect(dialog().locator('li').filter({ hasText: LABEL })).toContainText(
			'string 型は記録できません'
		);
		await expect(checkbox(RUN)).not.toBeChecked();
	});

	test('2. 新しいグループを作って登録すると、Hub 接続・グループ・タグができてダイアログが閉じる', async () => {
		await checkbox(RUN).check();
		// Hub 接続の下のグループがまだ無いので、既定は「新しいグループ」。
		await dialog().getByLabel('名前').fill(GROUP_NAME);
		await dialog().getByRole('button', { name: '2 件を登録' }).click();

		await expect(page.getByText('Hub のタグを 2 件登録しました。')).toBeVisible();
		await expect(dialog()).toHaveCount(0);

		const connections = page.locator('section.registry-section').nth(0).locator('div.list');
		await expect(connections.getByText('Hub 経由')).toBeVisible();
		const groups = page.locator('section.registry-section').nth(1).locator('div.list');
		await expect(groups.getByText(GROUP_NAME)).toBeVisible();
	});

	test('3. 登録済みは選べず、同じグループで名前が衝突するとその行に理由が出て何も登録されない', async () => {
		await page.unroute('**/api/hub/catalog');
		await routeCatalog(page, []);
		await page.getByRole('button', { name: 'Hub から追加' }).click();
		await expect(dialog()).toBeVisible();

		await expect(checkbox(TEMP)).toBeDisabled();
		await expect(dialog().locator('li').filter({ hasText: TEMP })).toContainText('登録済み');
		await expect(checkbox(RUN)).toBeDisabled();

		// 既存の Hub グループ（直前に作ったもの）が既定の登録先。
		await checkbox(OTHER_TEMP).check();
		await dialog().getByRole('button', { name: '1 件を登録' }).click();

		await expect(dialog().locator('li').filter({ hasText: OTHER_TEMP })).toContainText(
			'この収集グループ内では既に使用されています'
		);
		await expect(dialog()).toBeVisible();
		const tags = (await (await page.request.get('/api/tags', { headers })).json()) as Row[];
		expect(tags.filter((t) => t.address === OTHER_TEMP)).toEqual([]);
	});

	test('4. 未保存の選択がある間は Esc で閉じず、× は確認を出す', async () => {
		// 3 の続き（OTHER_TEMP を選んだまま）。
		await page.keyboard.press('Escape');
		await expect(dialog()).toBeVisible();

		confirmAnswer = 'dismiss';
		await dialog().getByRole('button', { name: '閉じる' }).click();
		await expect(dialog()).toBeVisible();
		expect(confirms.at(-1)).toContain('ダイアログを閉じますか');

		confirmAnswer = 'accept';
		await dialog().getByRole('button', { name: '閉じる' }).click();
		await expect(dialog()).toHaveCount(0);
		// フォーカスは開いた元のボタンへ戻る。
		await expect(page.getByRole('button', { name: 'Hub から追加' })).toBeFocused();
	});

	test('5. Hub 経由のタグの編集にスケーリングの欄が無く、Hub 接続の行は読み取り専用', async () => {
		const tagsSection = page.locator('section.registry-section').nth(2);
		await tagsSection.locator('div.list').getByText(TEMP).click();
		const detail = tagsSection.locator('div.detail');
		await expect(detail.getByText('Hub 経由のタグです')).toBeVisible();
		await expect(detail.getByLabel('スケーリング: 生値 下限')).toHaveCount(0);
		await expect(detail.getByLabel('Hub のタグ名（外部名）')).toHaveValue(TEMP);

		const connectionsSection = page.locator('section.registry-section').nth(0);
		await connectionsSection.locator('div.list').getByText('Hub 経由').click();
		const connectionDetail = connectionsSection.locator('div.detail');
		await expect(connectionDetail.getByText('（Hub 経由の接続）')).toBeVisible();
		await expect(connectionDetail.getByLabel('ホスト')).toHaveCount(0);
		await expect(connectionDetail.getByRole('button', { name: '削除' })).toBeVisible();
	});
});
