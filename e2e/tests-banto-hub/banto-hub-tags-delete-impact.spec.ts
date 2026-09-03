/**
 * タグ削除前の参照影響・完全外部名表示（T18-1、TAG-UX-C 5点目、
 * docs/banto-hub-desktop-plan.md §9.4「削除前に演算タグ等の参照影響と完全な
 * 外部名を表示する」）の実 DOM 受け入れテスト。`tagDeleteImpact.test.ts`
 * （vitest、純関数の単体テスト）ではまだ確認できていない「実 DOM で削除
 * ボタンを押したときに、`window.confirm` のメッセージに完全外部名と
 * 参照元の演算タグが実際に出るか」を確認する。
 *
 * `banto-hub-tags-dirty-confirm.spec.ts`/`banto-hub-tags-busy.spec.ts` と
 * 同じパターン: 別 `describe.serial` ブロック（別 `page`）、認証は
 * `ensureLoggedIn`、前提データ（PLC接続・収集グループ・PLCタグ・演算タグ）
 * は UI 操作ではなく `page.request` で直接 REST を叩いて作る。演算タグは
 * `calc` 予約接続（サーバー起動時に自動作成、
 * `crates/banto-tags/src/plc_connection.rs::CALC_CONNECTION_NAME`）配下の
 * 収集グループにしか作れないため、`GET /api/plc-connections` でその id を
 * 取得してから収集グループを作る。
 *
 * T19 S2-c2（UX-40、docs/banto-hub-t19-design.md §3.10）追記: タグ削除は
 * 「削除の遅延実行」になった（confirm 直後は一覧から消えるだけで、実際の
 * `DELETE` は数秒後）。テスト3（confirm OK → 削除完了）は見た目だけでなく
 * 実際の `DELETE` レスポンスを待つよう変更し、取り消しトースト
 * （ロール+ラベル `role: 'button', name: '取り消し'` で取得）を押すケースを
 * テスト4として追加した。
 */
import { expect, test, type Page, type Request } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-del-impact-plc';
const GROUP_NAME = 'e2e-del-impact-group';
// 削除確認テストの本題（参照元がある/ない）を分けるため、参照される
// タグと参照されないタグを分ける。
const REFERENCED_TAG_NAME = 'e2e-del-src';
const UNREFERENCED_TAG_NAME = 'e2e-del-noref';
const CALC_GROUP_NAME = 'e2e-del-impact-calc-group';
const COMPUTED_TAG_NAME = 'e2e-del-computed';

const REFERENCED_EXTERNAL_NAME = `${CONNECTION_NAME}.${GROUP_NAME}.${REFERENCED_TAG_NAME}`;
const UNREFERENCED_EXTERNAL_NAME = `${CONNECTION_NAME}.${GROUP_NAME}.${UNREFERENCED_TAG_NAME}`;
const COMPUTED_EXTERNAL_NAME = `calc.${CALC_GROUP_NAME}.${COMPUTED_TAG_NAME}`;

test.describe.serial('banto-hub タグ削除前の参照影響表示 (TAG-UX-C)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ1: シミュレーションモードの PLC接続 + 収集グループ +
		// 削除対象タグ2件（参照される側／参照されない側、実 PLC/実ネットワーク
		// へは繋がない）。
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

		for (const name of [REFERENCED_TAG_NAME, UNREFERENCED_TAG_NAME]) {
			const tagRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: {
					name,
					collectionGroupId: group.id,
					// modbus-tcp 接続配下なので Modbus 参照番号形式が必要
					// （dirty-confirm/busy spec と同じ理由）。
					address: name === REFERENCED_TAG_NAME ? '40001' : '40002',
					dataType: 'i16',
					decimals: 0,
					enabled: true,
					writable: false,
					tagKind: 'plc'
				}
			});
			expect(tagRes.ok()).toBe(true);
		}

		// 前提データ2: `calc` 予約接続（起動時に自動作成済み）配下の収集
		// グループに、`REFERENCED_TAG_NAME` の完全外部名を参照する演算タグを
		// 1件作る - `UNREFERENCED_TAG_NAME` はどの演算タグからも参照しない。
		const connectionsRes = await page.request.get('/api/plc-connections', {
			headers: authedHeaders
		});
		expect(connectionsRes.ok()).toBe(true);
		const connections = (await connectionsRes.json()) as { id: number; name: string }[];
		const calcConnection = connections.find((c) => c.name === 'calc');
		if (!calcConnection) {
			throw new Error("予約接続 'calc' が見つかりません（サーバー起動時に自動作成される想定）");
		}

		const calcGroupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: {
				name: CALC_GROUP_NAME,
				plcConnectionId: calcConnection.id,
				periodMs: 1000,
				enabled: true
			}
		});
		expect(calcGroupRes.ok()).toBe(true);
		const calcGroup = (await calcGroupRes.json()) as { id: number };

		const computedTagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: COMPUTED_TAG_NAME,
				collectionGroupId: calcGroup.id,
				address: '',
				dataType: 'f32',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'computed',
				// 単なる参照式 - REFERENCED_TAG_NAME の完全外部名をそのまま含む。
				expression: `${REFERENCED_EXTERNAL_NAME} * 2`,
				retain: false
			}
		});
		expect(computedTagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test.beforeEach(async () => {
		// 演算タグ作成後も `tags` state に載るよう、削除操作の前に必ず
		// `/tags` を（再）訪問する。
		await page.goto('/tags');
	});

	test('1. 参照元の演算タグがあるタグの削除確認: 完全外部名と参照元一覧が出る', async () => {
		await page.getByRole('gridcell', { name: REFERENCED_TAG_NAME, exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${REFERENCED_TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		let dialogMessage: string | null = null;
		page.once('dialog', (dialog) => {
			dialogMessage = dialog.message();
			void dialog.dismiss();
		});
		await drawer.getByRole('button', { name: '削除' }).click();

		await expect
			.poll(() => dialogMessage, { message: 'window.confirm が呼ばれること' })
			.not.toBeNull();

		expect(dialogMessage).toContain(REFERENCED_EXTERNAL_NAME);
		expect(dialogMessage).toContain(COMPUTED_EXTERNAL_NAME);
		expect(dialogMessage).toContain('参照');

		// dismiss したのでタグは削除されず、Drawer も開いたまま。
		await expect(drawer).toBeVisible();
	});

	test('2. 参照元が無いタグの削除確認: 完全外部名は出るが参照警告は出ない', async () => {
		await page.getByRole('gridcell', { name: UNREFERENCED_TAG_NAME, exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${UNREFERENCED_TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		let dialogMessage: string | null = null;
		page.once('dialog', (dialog) => {
			dialogMessage = dialog.message();
			void dialog.dismiss();
		});
		await drawer.getByRole('button', { name: '削除' }).click();

		await expect
			.poll(() => dialogMessage, { message: 'window.confirm が呼ばれること' })
			.not.toBeNull();

		expect(dialogMessage).toContain(UNREFERENCED_EXTERNAL_NAME);
		expect(dialogMessage).not.toContain('参照');

		await expect(drawer).toBeVisible();
	});

	test('3. 削除確認で OK すれば、参照元が無いタグは従来どおり削除される', async () => {
		await page.getByRole('gridcell', { name: UNREFERENCED_TAG_NAME, exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${UNREFERENCED_TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		page.once('dialog', (dialog) => {
			void dialog.accept();
		});

		// T19 S2-c2（UX-40、docs/banto-hub-t19-design.md §3.10）: 削除確認 OK
		// 後も、実際の削除は数秒（`UNDO_WINDOW_MS`）遅延実行される - 一覧から
		// 消える見た目だけでは「サーバー側でも実際に削除された」ことの
		// 証明にならない（#216 の教訓 - トースト文言だけの待ち合わせは古い
		// トーストに誤マッチしうる）。実際の DELETE リクエストの完了を
		// 待ってから完了とみなす。
		const deleteRequest = page.waitForResponse(
			(res) => res.request().method() === 'DELETE' && /\/api\/tags\/\d+$/.test(res.url())
		);
		await drawer.getByRole('button', { name: '削除' }).click();

		// ドロワーを閉じる・一覧から隠すのは confirm 直後（猶予中の
		// 楽観的な見た目）。
		await expect(drawer).toBeHidden();
		await expect(
			page.getByRole('gridcell', { name: UNREFERENCED_TAG_NAME, exact: true })
		).toHaveCount(0);

		// 猶予後、実際に DELETE が送られて完了することを確認する
		// （後続テスト/再実行のためのフィクスチャ後始末も兼ねる）。
		await deleteRequest;
	});

	test('4. 削除確認で OK 後に「取り消し」を押すと、猶予後もタグは削除されない', async () => {
		let deleteRequestSeen = false;
		const onRequest = (req: Request): void => {
			if (req.method() === 'DELETE' && /\/api\/tags\/\d+$/.test(req.url())) {
				deleteRequestSeen = true;
			}
		};
		page.on('request', onRequest);

		try {
			await page.getByRole('gridcell', { name: REFERENCED_TAG_NAME, exact: true }).click();
			const drawer = page.getByRole('dialog', { name: `${REFERENCED_TAG_NAME} を編集` });
			await expect(drawer).toBeVisible();

			page.once('dialog', (dialog) => {
				void dialog.accept();
			});
			await drawer.getByRole('button', { name: '削除' }).click();

			// confirm 直後は他の削除同様、ドロワーが閉じ一覧から消える
			// （猶予中の楽観的な見た目）。
			await expect(drawer).toBeHidden();
			await expect(
				page.getByRole('gridcell', { name: REFERENCED_TAG_NAME, exact: true })
			).toHaveCount(0);

			const undoButton = page.getByRole('button', { name: '取り消し' });
			await expect(undoButton).toBeVisible();
			await undoButton.click();

			// 取り消し後は即座に一覧へ戻る。
			await expect(
				page.getByRole('gridcell', { name: REFERENCED_TAG_NAME, exact: true })
			).toBeVisible();

			// 猶予（`UNDO_WINDOW_MS` = 6秒）を過ぎても実際には削除リクエストが
			// 送られていないこと、タグが（サーバー側の状態としても）残って
			// いることまで確認する。
			await page.waitForTimeout(7000);
			expect(deleteRequestSeen).toBe(false);

			await page.reload();
			await expect(
				page.getByRole('gridcell', { name: REFERENCED_TAG_NAME, exact: true })
			).toBeVisible();
		} finally {
			page.off('request', onRequest);
		}
	});
});
