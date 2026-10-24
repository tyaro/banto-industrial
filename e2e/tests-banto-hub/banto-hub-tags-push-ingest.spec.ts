/**
 * R2a-2e（docs/banto-hub-ingest-design.md §11 の R2a-2e の行）: 取り込み口 `push` の
 * 実 DOM 受け入れテスト。
 *
 * 固定する内容:
 * 1. 作成ウィザードでプロトコルを push にすると、接続先（ホスト・ポート）の代わりに
 *    受信のタイムアウトと DB ブリッジの区画が出る。ブリッジの送る量の見積もり
 *    （§7.1）が上限の 8 割を超えると**保存の前に赤く**示され、「次へ」に進めない。
 *    ブリッジを外すと作成できる。
 * 2. 再設定のドロワーに、送り手向けの URL（`/api/v1/ingest/{id}/…`）とスコープ
 *    （`ingest:#{id}`）、受信の状態（まだ送り手が無いので「送り手待ち」）が出る。
 * 3. `ingest:#{id}` のキーで REST の一括送信（`POST /api/v1/ingest/{id}/values`）で
 *    送った値が、モニタのタグ一覧に出る（収集を開始した状態で）。
 * 4. ドロワーの受信の状態が「受信中」になり、送り手（REST・キーの名前）が出る。
 *    キーの値・送った値は状態に出ない（サーバーが出さない。ここでは DOM で確かめる）。
 *
 * 前提データ（グループ・タグ・キー）は `page.request` で REST を直接叩いて作る
 * （`banto-hub-tags-db-source-connection.spec.ts` と同じ型）。同期点はトーストでは
 * なく `page.waitForResponse`（クリックの前に張る）。ファイル名は
 * `banto-hub-smoke.spec.ts` より辞書順で後（`tags` > `smoke`）。収集は
 * `start-all-simulation` で開始し（PLC の接続はシミュレータになる。push は
 * シミュレーションに切り替わらない - hub.rs の simulation の判定は modbus-tcp /
 * slmp だけ）、`afterAll` で必ず止める（後続の spec は停止中を前提にする）。
 */
import { expect, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const PUSH_CONN = `e2e-push-conn-${RUN_ID}`;
const PUSH_GROUP = `e2e-push-grp-${RUN_ID}`;
const PUSH_TAG = `e2e-push-tag-${RUN_ID}`;
const PUSH_KEY_NAME = `e2e-push-key-${RUN_ID}`;
const INGEST_KEY = 'line1.temp';
const EXTERNAL_NAME = `${PUSH_CONN}.${PUSH_GROUP}.${PUSH_TAG}`;

interface ConnectionRow {
	id: number;
	name: string;
	protocol: string;
}

interface ApiKeyRow {
	id: number;
	name: string;
	revokedAt?: number | null;
}

function escapeRegExp(value: string): string {
	return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** `banto-hub-tags-db-source-connection.spec.ts` の同名関数と同じ理由で前方一致を避ける。 */
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
		for (const c of connections.filter((c) => c.name === PUSH_CONN)) {
			// 接続の削除は配下のグループ・タグごと消す（T19 S2-b のカスケード）。
			await request.delete(`/api/plc-connections/${c.id}`, { headers });
		}
	}
	const keysRes = await request.get('/api/api-keys', { headers });
	if (keysRes.ok()) {
		const keys = (await keysRes.json()) as ApiKeyRow[];
		for (const k of keys.filter((k) => k.name === PUSH_KEY_NAME && !k.revokedAt)) {
			await request.post(`/api/api-keys/${k.id}/revoke`, { headers });
		}
	}
}

test.describe.serial('banto-hub 取り込み口 push（R2a-2e）', () => {
	// R2b-6b（#813、2026-10-24 オーナー決定）: REST が settings の形になり、今の接続の画面は 6c まで
	// 接続を保存できない。6c（DriverSettingsForm への置き換え）の PR で必ず外す。
	test.fixme(true, 'R2b-6c で接続の画面が settings の形に追従するまで止める');
	let page: Page;
	let headers: Record<string, string>;
	let connectionId: number;
	let groupId: number;
	let ingestKey: string;
	let startedCollection = false;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		await cleanupFixtures(page.request, headers);
	});

	test.afterAll(async () => {
		if (startedCollection) {
			await page.request.post('/api/collection/stop', { headers });
		}
		await cleanupFixtures(page.request, headers);
		await page.close();
	});

	test('1. 作成ウィザード: push は接続先の代わりにタイムアウトとブリッジを出し、上限を超える見積もりは保存の前に赤い', async () => {
		await page.goto('/tags');
		await page.getByRole('button', { name: 'PLC接続を追加', exact: true }).click();
		const wizard = page.getByRole('dialog', { name: '新規作成', exact: true });
		await expect(wizard).toBeVisible();
		await wizard.locator('#connection-name').fill(PUSH_CONN);
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();

		await wizard.getByLabel('プロトコル').selectOption({ value: 'push' });
		await expect(wizard.getByLabel('ホスト')).toHaveCount(0);
		await expect(wizard.getByLabel('ポート')).toHaveCount(0);
		await expect(wizard.getByText('シミュレーションモード')).toHaveCount(0);
		await wizard.getByLabel('受信のタイムアウト（ms）').fill('60000');

		// ブリッジ: 100 ms のクエリ 3 本（30 メッセージ/秒）は上限（16）を超える。
		await wizard.getByLabel('DB ブリッジ（PostgreSQL を読んで送る）').check();
		for (let i = 0; i < 3; i++) {
			await wizard.getByRole('button', { name: 'クエリを追加', exact: true }).click();
			const query = wizard.getByRole('group', { name: `クエリ ${i + 1}`, exact: true });
			await query.getByLabel('周期').selectOption({ label: '100 ms' });
			await query.getByLabel('SQL').fill('SELECT 1 AS v');
			await query.getByLabel('列とキーの対応（1 行に 1 つ「列名=キー」）').fill(`v=k${i}`);
		}
		const estimate = wizard.getByTestId('bridge-load-estimate');
		await expect(estimate).toContainText('約 30 メッセージ/秒・30 件/秒');
		await expect(estimate).toHaveClass(/over/);
		await expect(wizard.getByRole('alert')).toContainText(
			'上限（16 メッセージ・4,000 件）を超えます'
		);
		await expect(wizard.getByRole('button', { name: '次へ', exact: true })).toBeDisabled();

		// 周期を 1 本だけ 1000 ms にすると 21 → まだ超える。2 本を 1000 ms で 12 に収まる。
		await wizard
			.getByRole('group', { name: 'クエリ 1', exact: true })
			.getByLabel('周期')
			.selectOption({ label: '1000 ms' });
		await wizard
			.getByRole('group', { name: 'クエリ 2', exact: true })
			.getByLabel('周期')
			.selectOption({ label: '1000 ms' });
		await expect(estimate).toContainText('約 12 メッセージ/秒');
		await expect(estimate).not.toHaveClass(/over/);

		// この E2E には PostgreSQL 接続が無いので、ブリッジは外して作る。
		await wizard.getByLabel('DB ブリッジ（PostgreSQL を読んで送る）').uncheck();
		await expect(wizard.getByTestId('bridge-load-estimate')).toHaveCount(0);
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();

		await expect(wizard.getByText('60000 ms')).toBeVisible();
		await expect(wizard.getByText('なし（スクリプトなどの送り手が送る）')).toBeVisible();
		await expect(wizard.getByText(/作成すると、ここに送り手向けの URL/)).toBeVisible();

		const created = page.waitForResponse(
			(r) => r.url().endsWith('/api/plc-connections') && r.request().method() === 'POST'
		);
		await wizard.getByRole('button', { name: '作成', exact: true }).click();
		const response = await created;
		expect(response.ok()).toBe(true);
		const body = (await response.json()) as ConnectionRow & {
			settings: { stale_timeout_ms?: number } | null;
		};
		expect(body.protocol).toBe('push');
		expect(body.settings).toEqual({ stale_timeout_ms: 60000 });
		connectionId = body.id;
	});

	test('2. 再設定のドロワー: 送り手向けの URL・スコープと「送り手待ち」が出る', async () => {
		const groupRes = await page.request.post('/api/collection-groups', {
			headers,
			data: { name: PUSH_GROUP, plcConnectionId: connectionId, periodMs: 500, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		groupId = ((await groupRes.json()) as { id: number }).id;
		const tagRes = await page.request.post('/api/tags', {
			headers,
			data: {
				name: PUSH_TAG,
				collectionGroupId: groupId,
				address: INGEST_KEY,
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok(), await tagRes.text()).toBe(true);

		await page.goto('/tags');
		await connectionNodeByName(page, PUSH_CONN).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '接続を再設定', exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${PUSH_CONN} を編集`, exact: true });
		await expect(drawer).toBeVisible();
		await expect(drawer.getByTestId('ingest-rest-url')).toContainText(
			`/api/v1/ingest/${connectionId}/values`
		);
		await expect(drawer.getByTestId('ingest-ws-url')).toContainText(
			`/api/v1/ingest/${connectionId}/stream`
		);
		await expect(drawer.getByTestId('ingest-scope')).toHaveText(`ingest:#${connectionId}`);
		await expect(drawer.getByTestId('ingest-link')).toHaveText('送り手待ち');
		await page.keyboard.press('Escape');
	});

	test('3. ingest:#{id} のキーで REST で送った値がモニタのタグ一覧に出る', async () => {
		const keyRes = await page.request.post('/api/api-keys', {
			headers,
			data: { name: PUSH_KEY_NAME, scopes: [`ingest:#${connectionId}`] }
		});
		expect(keyRes.status(), await keyRes.text()).toBe(201);
		ingestKey = ((await keyRes.json()) as { key: string }).key;

		const startRes = await page.request.post('/api/collection/start-all-simulation', { headers });
		expect(startRes.ok(), await startRes.text()).toBe(true);
		startedCollection = true;

		await page.goto(`/monitor?group=${groupId}`);
		const row = page.locator('tr', { hasText: EXTERNAL_NAME });
		await expect(row).toBeVisible();
		// 開始の直後は計画の反映と WS の購読が追いつくまで待つ。送り直しながら待つ
		// （受信のタイムアウトは 60 秒なので、1 回届けば十分だが、収集の開始より
		// 前に届いた値も一般の送り手なら使われる - §4.5）。
		await expect(async () => {
			const sent = await page.request.post(`/api/v1/ingest/${connectionId}/values`, {
				headers: { Authorization: `Bearer ${ingestKey}` },
				data: { items: [{ key: INGEST_KEY, v: 42 }] }
			});
			expect(sent.status(), await sent.text()).toBe(200);
			const ack = (await sent.json()) as { accepted: number; collecting: boolean };
			expect(ack.accepted).toBe(1);
			expect(ack.collecting).toBe(true);
			// 外部名に RUN_ID（数字）が入るので、行全体ではなく値の列で見る。
			await expect(row.locator('td.value')).toHaveText('42', { timeout: 3000 });
		}).toPass({ timeout: 30_000 });
	});

	test('4. ドロワーの受信の状態が「受信中」になり、送り手（REST・キーの名前）が出る', async () => {
		await page.request.post(`/api/v1/ingest/${connectionId}/values`, {
			headers: { Authorization: `Bearer ${ingestKey}` },
			data: {
				items: [
					{ key: INGEST_KEY, v: 43 },
					{ key: 'line1.tmp', v: 1 }
				]
			}
		});
		await page.goto('/tags');
		await connectionNodeByName(page, PUSH_CONN).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '接続を再設定', exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${PUSH_CONN} を編集`, exact: true });
		const status = drawer.getByTestId('ingest-status');
		await expect(status.getByTestId('ingest-link')).toHaveText('受信中');
		await expect(status).toContainText(`REST: ${PUSH_KEY_NAME}`);
		await expect(status).toContainText('収集中');
		await expect(status).toContainText('line1.tmp');
		// 状態はキーの値を出さない（送った値も出さない - サーバー側のテスト
		// `the_status_ingest_section_reports_the_sender_without_secrets_or_values` で固定。
		// DOM では時刻の数字と紛れるので値の文字列では確かめない）。
		await expect(status).not.toContainText(ingestKey);
		await page.keyboard.press('Escape');
	});
});
