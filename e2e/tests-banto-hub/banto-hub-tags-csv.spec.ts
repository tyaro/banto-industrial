/**
 * CSV 新規/更新インポート + テンプレート/エクスポート DL（T18-3d、
 * docs/banto-hub-t18-design.md「T18-3d CSV 新規/更新分離＋テンプレート」、
 * TAG-UX-F）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-form.spec.ts`/`banto-hub-tags-revision.spec.ts` と同じ
 * パターン: 別 `describe.serial` ブロック（別 `page`）、認証・前提データは
 * `page.request` で直接 REST を叩いて作る（`simulation: true`、実 PLC 不要）。
 * CSV ファイルは `setInputFiles` に Buffer で流し込み、列は `TAG_CSV_COLUMNS`
 * （`$lib/banto/tagCsv.ts`）に一致させる。接続/グループは名前で参照するため、
 * CSV には事前作成した接続/グループ名を入れる。共有 DB を壊さないよう固定名は
 * `RUN_ID` で一意化し、リトライ再走用の冪等掃除を持つ。
 */
import { Buffer } from 'node:buffer';
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-csv-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-csv-group-${RUN_ID}`;

// 新規追加モードで作るタグ。
const NEW_TAG_A = `e2e-csv-new-a-${RUN_ID}`;
const NEW_TAG_B = `e2e-csv-new-b-${RUN_ID}`;
// 既存更新モード用に事前作成するタグ。
const UPD_CHANGED_TAG = `e2e-csv-upd-changed-${RUN_ID}`;
const UPD_UNCHANGED_TAG = `e2e-csv-upd-unchanged-${RUN_ID}`;
const UPD_ADDED_TAG = `e2e-csv-upd-added-${RUN_ID}`;
const UPDATED_UNIT = 'updated-unit';

// `TAG_CSV_COLUMNS`（$lib/banto/tagCsv.ts）と同じ列順・列名。
const CSV_HEADER = [
	'connection',
	'group',
	'name',
	'address',
	'dataType',
	'stringLength',
	'unit',
	'decimals',
	'rawLo',
	'rawHi',
	'engLo',
	'engHi',
	'thresholdH',
	'thresholdHh',
	'thresholdL',
	'thresholdLl',
	'enabled',
	'writable',
	'tagKind',
	'expression',
	'retain'
];

type CsvOverrides = Partial<Record<(typeof CSV_HEADER)[number], string>>;

/** 既定値（新規plcタグ）に上書きを適用した1行を、列順どおりの配列で返す。 */
function csvRow(overrides: CsvOverrides): string[] {
	const base: Record<string, string> = {
		connection: CONNECTION_NAME,
		group: GROUP_NAME,
		name: '',
		address: '',
		dataType: 'i16',
		stringLength: '',
		unit: '',
		decimals: '0',
		rawLo: '',
		rawHi: '',
		engLo: '',
		engHi: '',
		thresholdH: '',
		thresholdHh: '',
		thresholdL: '',
		thresholdLl: '',
		enabled: 'true',
		writable: 'false',
		tagKind: 'plc',
		expression: '',
		retain: 'false'
	};
	const merged = { ...base, ...overrides };
	return CSV_HEADER.map((c) => merged[c]);
}

/** ヘッダ + 行を RFC4180 の CRLF で連結する（本 spec の値はカンマ/引用符を含まない）。 */
function buildCsv(rows: string[][]): Buffer {
	const lines = [CSV_HEADER, ...rows].map((r) => r.join(','));
	return Buffer.from(lines.join('\r\n') + '\r\n', 'utf-8');
}

interface TagResponse {
	id: number;
	name: string;
	unit: string | null;
	collectionGroupId: number;
	revision: number;
}

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

test.describe.serial('banto-hub CSV 新規/更新インポート (T18-3d)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let groupId: number;

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

		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		groupId = ((await groupRes.json()) as { id: number }).id;

		// 更新モードの突き合わせ対象（unit 未設定で作り、片方だけ CSV で変える）。
		for (const [name, address] of [
			[UPD_CHANGED_TAG, '40101'],
			[UPD_UNCHANGED_TAG, '40102']
		] as const) {
			const tagRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: {
					name,
					collectionGroupId: groupId,
					address,
					dataType: 'i16',
					decimals: 0,
					enabled: true,
					writable: false,
					tagKind: 'plc'
				}
			});
			expect(tagRes.ok()).toBe(true);
		}
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec が仮想化グリッドで自分の行を
		// 見失わないよう、作った接続/グループ/タグは実行後に片付ける）。
		await cleanupExistingFixtures(page.request, authedHeaders);
		await page.close();
	});

	async function fetchTags(): Promise<TagResponse[]> {
		const res = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(res.ok()).toBe(true);
		return (await res.json()) as TagResponse[];
	}

	test('1. 新規追加: CSV をプレビュー→検証→登録すると新タグが作成される', async () => {
		await page.goto('/tags');
		await page.getByRole('button', { name: 'CSVインポート' }).click();
		const drawer = page.getByRole('dialog', { name: 'CSVインポート' });
		await expect(drawer).toBeVisible();

		const csv = buildCsv([
			csvRow({ name: NEW_TAG_A, address: '40010' }),
			csvRow({ name: NEW_TAG_B, address: '40011' })
		]);
		await drawer
			.locator('input[type="file"]')
			.setInputFiles({ name: 'new.csv', mimeType: 'text/csv', buffer: csv });

		await expect(drawer.getByRole('heading', { name: 'プレビュー（2件）' })).toBeVisible();

		await drawer.getByRole('button', { name: '検証', exact: true }).click();
		await expect(page.getByText('検証OK: 2件登録できます')).toBeVisible();

		await drawer.getByRole('button', { name: '登録', exact: true }).click();
		await expect(page.getByText('2件登録しました')).toBeVisible();

		const tags = await fetchTags();
		expect(tags.some((t) => t.name === NEW_TAG_A && t.collectionGroupId === groupId)).toBe(true);
		expect(tags.some((t) => t.name === NEW_TAG_B && t.collectionGroupId === groupId)).toBe(true);
	});

	test('2. 既存更新: 差分プレビュー（追加/変更/変更なし）→検証→適用で changed だけ反映', async () => {
		await page.goto('/tags');
		await page.getByRole('button', { name: 'CSVインポート' }).click();
		const drawer = page.getByRole('dialog', { name: 'CSVインポート' });
		await expect(drawer).toBeVisible();

		await drawer.getByTestId('tag-csv-mode-update').click();

		// changed（unit を変更）/ unchanged（既存と完全一致）/ added（既存に無い名前）。
		const csv = buildCsv([
			csvRow({ name: UPD_CHANGED_TAG, address: '40101', unit: UPDATED_UNIT }),
			csvRow({ name: UPD_UNCHANGED_TAG, address: '40102' }),
			csvRow({ name: UPD_ADDED_TAG, address: '40109' })
		]);
		await drawer
			.locator('input[type="file"]')
			.setInputFiles({ name: 'update.csv', mimeType: 'text/csv', buffer: csv });

		const summary = drawer.getByTestId('tag-csv-update-summary');
		await expect(summary).toContainText('追加 1');
		await expect(summary).toContainText('変更 1');
		await expect(summary).toContainText('変更なし 1');
		await expect(summary).toContainText('エラー 0');

		await drawer.getByTestId('tag-csv-update-validate').click();
		await expect(page.getByText('検証OK: 1件更新できます')).toBeVisible();

		await drawer.getByTestId('tag-csv-update-apply').click();
		await expect(page.getByText('1件更新しました')).toBeVisible();

		const tags = await fetchTags();
		// changed: unit が更新される。
		expect(tags.find((t) => t.name === UPD_CHANGED_TAG)?.unit).toBe(UPDATED_UNIT);
		// unchanged: 触れられない（unit は未設定のまま）。
		expect(tags.find((t) => t.name === UPD_UNCHANGED_TAG)?.unit ?? null).toBeNull();
		// added: 既存更新モードでは登録されない（暗黙の新規作成なし）。
		expect(tags.some((t) => t.name === UPD_ADDED_TAG)).toBe(false);
	});

	test('3. テンプレート DL: 列ヘッダのみの CSV をダウンロードできる', async () => {
		await page.goto('/tags');
		await page.getByRole('button', { name: 'CSVインポート' }).click();
		const drawer = page.getByRole('dialog', { name: 'CSVインポート' });
		await expect(drawer).toBeVisible();

		const [download] = await Promise.all([
			page.waitForEvent('download'),
			drawer.getByTestId('tag-csv-template-download').click()
		]);
		expect(download.suggestedFilename()).toBe('banto-hub-tags-template.csv');
	});

	test('4. エクスポート: 出力範囲「全件」で CSV をダウンロードできる', async () => {
		await page.goto('/tags');
		await page.getByTestId('tag-csv-export-scope').selectOption('all');

		const [download] = await Promise.all([
			page.waitForEvent('download'),
			page.getByRole('button', { name: 'CSVエクスポート' }).click()
		]);
		expect(download.suggestedFilename()).toMatch(/^banto-hub-tags-\d{4}-\d{2}-\d{2}\.csv$/);
	});
});
