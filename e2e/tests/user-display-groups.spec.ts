/**
 * グループ設定画面（`/groups`、#393 = #524 の段階 1）の E2E 固定。
 *
 * 固定したい受入条件:
 * 1. 表示グループを作成できる（名前・表示種別・トレンドの既定時間窓・ペン 2 本・
 *    ペンの色）。各ペンにはタグのしきい値が読み取り専用で出る（このスペックの
 *    タグはしきい値が無いので「しきい値なし」）。
 * 2. 再読み込みしても、作った内容がそのまま開ける（サーバーに保存されている）。
 * 3. 編集（種別の変更・ペンを外す）を保存し、再読み込み後も残る。
 * 4. ペンに割り当てたタグは削除できず、理由に参照元のグループ名が出る
 *    （§3.7.9 の 8。REST で確かめる）。
 * 5. 編集中に別の画面へ移ると確認が出る（#508 の未保存の確認）。「留まる」で
 *    入力が残る。
 * 6. 削除すると一覧から消え、再読み込みしても戻らない。外したあとはタグも消せる。
 *
 * ファイル名について: 管理者アカウントは `smoke.spec.ts` が作るので、辞書順で
 * それより後、かつログアウトで終わる `user-unsaved-changes.spec.ts` より前に
 * なる名前にする（`user-d...`）。
 *
 * フィクスチャ（PLC 接続・収集グループ・タグ 2 本）は REST で作り、`beforeAll`
 * の先頭と `afterAll` の両方で名前から掃除する（チェックリスト §3）。表示
 * グループを先に消さないとタグを消せない（4 の拒否）ので、掃除もその順。
 */
import { expect, test, type APIRequestContext, type Dialog, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
type ApiHeaders = Record<string, string>;

const CONNECTION_NAME = 'E2E-DG-PLC';
const COLLECTION_GROUP_NAME = 'E2E-DG-収集';
const TAG_A = 'E2E-DG-タグA';
const TAG_B = 'E2E-DG-タグB';
const GROUP_NAME = 'E2E-DG-ライン1';
const CONFIRM_MESSAGE = '保存していない変更があります。変更を破棄してこの画面から移動しますか？';

interface NamedRow {
	id: number;
	name: string;
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

async function getList(
	request: APIRequestContext,
	headers: ApiHeaders,
	url: string
): Promise<NamedRow[]> {
	const res = await request.get(url, { headers });
	if (!res.ok()) throw new Error(`GET ${url} が ${res.status()}: ${await res.text()}`);
	return (await res.json()) as NamedRow[];
}

async function postJson(
	request: APIRequestContext,
	headers: ApiHeaders,
	url: string,
	data: unknown
): Promise<NamedRow> {
	const res = await request.post(url, { headers, data });
	if (!res.ok()) throw new Error(`POST ${url} が ${res.status()}: ${await res.text()}`);
	return (await res.json()) as NamedRow;
}

/** このスペックが作ったものを名前で探して消す（表示グループ → タグ → 収集グループ → 接続）。 */
async function cleanupFixtures(request: APIRequestContext, headers: ApiHeaders): Promise<void> {
	const failures: string[] = [];
	const deleteByName = async (listUrl: string, names: string[]): Promise<void> => {
		try {
			const rows = await getList(request, headers, listUrl);
			for (const row of rows.filter((r) => names.includes(r.name))) {
				const res = await request.delete(`${listUrl}/${row.id}`, { headers });
				if (res.status() !== 204 && res.status() !== 404) {
					failures.push(`DELETE ${listUrl}/${row.id} が ${res.status()}: ${await res.text()}`);
				}
			}
		} catch (err) {
			failures.push(err instanceof Error ? err.message : String(err));
		}
	};
	await deleteByName('/api/display-groups', [GROUP_NAME]);
	await deleteByName('/api/tags', [TAG_A, TAG_B]);
	await deleteByName('/api/collection-groups', [COLLECTION_GROUP_NAME]);
	await deleteByName('/api/plc-connections', [CONNECTION_NAME]);
	if (failures.length > 0) throw new Error(`掃除に失敗しました:\n${failures.join('\n')}`);
}

test.describe.serial('chronogazer グループ設定画面（#393）', () => {
	let page: Page;
	let headers: ApiHeaders;
	let tagAId: number;
	let dialogs: string[] = [];
	let answer: 'stay' | 'leave' = 'leave';

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		page.on('dialog', (dialog: Dialog) => {
			dialogs.push(dialog.message());
			void (answer === 'leave' ? dialog.accept() : dialog.dismiss());
		});
		headers = await fetchApiHeaders(page.request);
		await cleanupFixtures(page.request, headers);

		const conn = await postJson(page.request, headers, '/api/plc-connections', {
			name: CONNECTION_NAME,
			protocol: 'modbus-tcp',
			host: '127.0.0.1',
			port: 502,
			unitId: 1,
			enabled: false,
			wordOrder: ''
		});
		const group = await postJson(page.request, headers, '/api/collection-groups', {
			name: COLLECTION_GROUP_NAME,
			plcConnectionId: conn.id,
			periodMs: 1000,
			enabled: true
		});
		for (const [name, address] of [
			[TAG_A, '40001'],
			[TAG_B, '40002']
		]) {
			const tag = await postJson(page.request, headers, '/api/tags', {
				name,
				collectionGroupId: group.id,
				address,
				dataType: 'u16',
				decimals: 0,
				enabled: true
			});
			if (name === TAG_A) tagAId = tag.id;
		}

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

	test.beforeEach(() => {
		dialogs = [];
		answer = 'leave';
	});

	const editor = () => page.getByRole('region', { name: '表示グループの編集' });
	const list = () => page.getByRole('region', { name: '表示グループの一覧' });
	// 行の「上へ」「下へ」もグループ名を含むので、開くボタンだけに絞る。
	const groupButton = () => list().locator('button.group-open', { hasText: GROUP_NAME });

	async function openGroup(): Promise<void> {
		await groupButton().click();
		await expect(editor().getByLabel('名前')).toHaveValue(GROUP_NAME);
	}

	test('1. 表示グループを作成できる（ペン 2 本・種別・時間窓・色、しきい値は読み取り専用）', async () => {
		await page
			.locator('aside nav')
			.getByRole('link', { name: /グループ設定/ })
			.click();
		await expect(page).toHaveURL(/\/groups$/);
		await expect(page.getByRole('heading', { level: 2, name: 'グループ設定' })).toBeVisible();

		await list().getByRole('button', { name: '新規グループ' }).click();
		await editor().getByLabel('名前').fill(GROUP_NAME);
		await editor().getByLabel('表示種別').selectOption({ label: 'トレンド' });
		await editor().getByLabel('既定の時間窓').selectOption({ label: '30 分' });
		await editor().getByRole('button', { name: 'ペンを追加' }).click();
		await editor().getByRole('button', { name: 'ペンを追加' }).click();
		await editor().getByLabel('ペン 1 のタグ').selectOption({ label: TAG_A });
		await editor().getByLabel('ペン 2 のタグ').selectOption({ label: TAG_B });
		await editor().getByLabel('ペン 2 の色').selectOption({ label: '色 5' });
		await expect(editor().getByText('しきい値: しきい値なし')).toHaveCount(2);
		await expect(editor().locator('.banto-unsaved')).toHaveText('未保存の変更があります');

		await editor().getByRole('button', { name: '作成' }).click();
		await expect(groupButton()).toContainText('トレンド・ペン 2 本');
		await expect(editor().locator('.banto-unsaved')).toHaveCount(0);
	});

	test('2. 再読み込みしても作った内容のまま開ける', async () => {
		await page.reload();
		await openGroup();
		await expect(editor().getByLabel('表示種別')).toHaveValue('trend');
		await expect(editor().getByLabel('既定の時間窓')).toHaveValue('1800');
		await expect(editor().getByLabel('ペン 1 のタグ')).toHaveValue(String(tagAId));
		await expect(editor().getByLabel('ペン 2 のタグ').locator('option:checked')).toHaveText(TAG_B);
		await expect(editor().getByLabel('ペン 2 の色')).toHaveValue('5');
	});

	test('3. 編集（種別を計器へ・ペン 2 を外す）を保存し、再読み込み後も残る', async () => {
		await editor().getByLabel('表示種別').selectOption({ label: '計器' });
		await expect(editor().getByLabel('既定の時間窓')).toHaveCount(0);
		await editor().getByRole('button', { name: 'ペン 2 を外す' }).click();
		await editor().getByRole('button', { name: '保存' }).click();
		await expect(groupButton()).toContainText('計器・ペン 1 本');

		await page.reload();
		await openGroup();
		await expect(editor().getByLabel('表示種別')).toHaveValue('gauge');
		await expect(editor().getByLabel('ペン 1 のタグ')).toHaveValue(String(tagAId));
		await expect(editor().getByLabel('ペン 2 のタグ')).toHaveCount(0);
	});

	test('4. ペンに割り当てたタグは削除できず、参照元のグループ名が理由に出る', async () => {
		const res = await page.request.delete(`/api/tags/${tagAId}`, { headers });
		expect(res.status(), await res.text()).toBeGreaterThanOrEqual(400);
		const body = (await res.json()) as {
			kind: string;
			field_errors: { field: string; message: string }[];
		};
		expect(body.kind).toBe('validation');
		expect(body.field_errors[0].field).toBe('displayGroups');
		expect(body.field_errors[0].message).toContain(`「${GROUP_NAME}」`);
		const still = await page.request.get(`/api/tags/${tagAId}`, { headers });
		expect(still.status()).toBe(200);
	});

	test('5. 編集中に別の画面へ移ると確認が出て、「留まる」で入力が残る', async () => {
		answer = 'stay';
		await editor().getByLabel('名前').fill(`${GROUP_NAME}-未保存`);
		await page.locator('aside nav').getByRole('link', { name: /監視/ }).click();
		await expect.poll(() => dialogs).toEqual([CONFIRM_MESSAGE]);
		await expect(page).toHaveURL(/\/groups$/);
		await expect(editor().getByLabel('名前')).toHaveValue(`${GROUP_NAME}-未保存`);

		await editor().getByRole('button', { name: '変更を取り消す' }).click();
		await expect(editor().getByLabel('名前')).toHaveValue(GROUP_NAME);
		await expect(editor().locator('.banto-unsaved')).toHaveCount(0);
	});

	test('6. 削除すると一覧から消え、再読み込みしても戻らない。外したタグは消せる', async () => {
		answer = 'leave';
		await editor().getByRole('button', { name: '削除' }).click();
		await expect(groupButton()).toHaveCount(0);
		expect(dialogs).toEqual([`${GROUP_NAME} を削除しますか？`]);

		await page.reload();
		await expect(page.getByRole('heading', { level: 2, name: 'グループ設定' })).toBeVisible();
		await expect(list().getByRole('button', { name: '新規グループ' })).toBeEnabled();
		await expect(groupButton()).toHaveCount(0);

		const res = await page.request.delete(`/api/tags/${tagAId}`, { headers });
		expect(res.status(), await res.text()).toBe(204);
	});
});
