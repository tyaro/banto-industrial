/**
 * 「記録」ページ（`/tags`）の E2E 固定（#383 段階2a / R1-B）。
 *
 * 固定したい受入条件:
 * 1. 管理者で `/tags` を開き、PLC接続 → 収集グループ → タグ の順に作成でき、
 *    それぞれ一覧に出ること（FK の依存順どおり、直前に作った行が次の
 *    セクションのプルダウンに現れること込み）。
 * 2. 検証エラーが人間可読で出ること - `banto_tags` の UNIQUE 制約
 *    （`plc_connections.name` は全体で一意、`crates/banto-tags/src/support.rs`
 *    の `map_write_error`）を、同名の PLC接続を2つ作ろうとして踏み、
 *    「既に使用されています」が名前欄のすぐ下に出ることを確認する。
 *    （`periodMs` は固定選択肢の `<select>` なので範囲外の値を選びようが
 *    無い。名前の重複は「サーバー側検証がフィールドへ人間可読で返る」経路を、
 *    実際に踏める形で固定したもの。`address` の形式エラーは #414 段階1 で
 *    踏めるようになったので、下の受入条件 8 で別に固定する。）
 * 3. viewer は閲覧のみで、新規作成フォーム・削除ボタンが一切出ないこと。
 * 4.（#391レビュー B の回帰固定）スケーリングを部分指定（生値下限だけ）で
 *    タグを作成しようとすると、`crates/banto-tags/src/scaling.rs`の
 *    `Scaling::from_parts()`が返す`field: "scaling"`のエラーが、対応する
 *    フォーム項目（生値/工学値の4項目）に人間可読な理由として出ること
 *    （修正前は画面にもトーストにも何も出ず、保存できない理由が
 *    分からなかった）。作成のみを固定する（編集も同じ`applyServerErrors`
 *    経路を通るため、作成側の固定で判断ロジックの回帰は拾える - 編集は
 *    行選択やレジストリ状態の前提が増えて重くなるため見送った）。
 * 5.（#391レビュー C の回帰固定）収集グループが残っている PLC接続の削除は
 *    拒否され、「この接続を使用している収集グループがN件あるため削除
 *    できません」という具体的な理由がトーストに出ること（修正前は
 *    `ProviderError.message`が一律`"validation failed"`になり理由が
 *    消えていた）。データも消えていないことを一覧で確認する。
 * 6.（#394レビュー P2 の回帰固定）削除の応答待ち（`page.route`で`DELETE`を
 *    保留するゲート - 作法は`e2e/tests-banto-hub/banto-hub-tags-busy.spec.ts`
 *    に倣う）の間、編集フォームの入力欄・保存ボタン・削除ボタンが無効化
 *    されること、保存を試みても`PUT`が送信されないこと（修正前は
 *    `submitting`に削除中フラグが入っておらず、削除の`await`中に同じ行へ
 *    `PUT`を送れてしまっていた）。削除が検証エラーで拒否された場合は、
 *    具体的な理由が表示され、編集操作が再び有効に戻ることも確認する
 *    （子を持つ PLC接続の削除を既存の拒否経路として使う）。PLC接続でのみ
 *    固定する - 収集グループ・タグの`saveX`/`submitting`は同じ3行の形
 *    （`savingX || deletingX`）で入っており、差分レビューで確認済み
 *    （理由は本テストのdoc comment参照）。
 * 7.（#394レビュー P1-3 の回帰固定）一覧の取得に失敗したとき、0行の
 *    グリッドとして描かれず、「先にPLC接続を1件以上作成してください」という
 *    **誤った指示**が出ないこと。`page.route` で `GET /api/plc-connections`
 *    だけを落とし（作法は`e2e/tests-banto-hub/banto-hub-tags-busy.spec.ts`と
 *    `banto-hub-tags-load-state.spec.ts`に倣う）、失敗が分かる表示と再試行の
 *    導線が出ること、route を外して再試行すれば復旧することを確認する
 *    （修正前はトーストが消えたあと「1件も登録されていない」という永続的な
 *    誤表示になり、利用者を重複登録へ誘導していた）。PLC接続でのみ固定する -
 *    収集グループ・タグの3状態は同じ純関数（`tagsPageLogic.ts`の
 *    `listSectionView`/`createFormGate`）を同じ形で通しており、その総当たりは
 *    vitest 側で固定してある。
 * 8.（#414 段階1 の回帰固定）接続のプロトコルで読めないアドレスのタグは
 *    保存できず、理由（Modbus の参照番号として読めないこと）が
 *    デバイスアドレス欄に出ること。修正前は Modbus 接続の下に MELSEC 表記の
 *    `D3000` を保存でき、収集開始が全体ごと失敗していた。このため 5・6 で
 *    作るタグのアドレスも Modbus の参照番号（`40001`/`40011`）にしてある
 *    （テストの意図 = CRUD とスケーリングの検証エラーの表示は変わらない）。
 *    テスト `6b.` で固定する。
 * 9.（#525 → #532）しきい値（LL/L/H/HH）を画面から設定でき、再読み込みしても
 *    残ること、大小関係が崩れると理由が欄に出ること（`5b.`）。#532 から
 *    しきい値はタグ定義ではなく**記録計の側の設定**で、タグの編集ペインの下の
 *    「しきい値（記録計の設定）」のフォームで**別に保存する**（「しきい値を保存」）。
 *    タグの「保存」はしきい値を消さない（`5b.`）。別の画面が先に保存した
 *    しきい値を古い版で保存すると、再読み込みを促す案内が出て上書きされない
 *    こと（楽観ロック、`5c.`）。
 *
 * ファイル名について: `smoke.spec.ts` が初回セットアップ（管理者アカウント
 * 作成）を実 DOM で行うため、このファイルは辞書順でそれより後でなければ
 * ならない（`playwright.config.ts` は `workers: 1`/`fullyParallel: false` で
 * `testDir` 配下をファイル名の辞書順に実行する - `user-settings-hub.spec.ts`
 * の doc comment にある罠と同じ）。`tags` は `smoke` より後、`user-*` より
 * 前に入る（`s` < `t` < `u`）。
 *
 * 10.（#1179 PR4b）3 ペイン（左ツリー・中央タブ・右の詳細）: ツリーで選ぶと
 *    一覧が絞られる（`12.`）、URL を開き直すと同じ対象とタブに戻る（`13.`）、
 *    未保存のままタブ・対象を変えると確認が出てキャンセルで残る（`14.`）、
 *    900px 以下で左がドロワーになり選択中の名前が帯に残る（`15.`）。
 *
 * #1179 PR4b からページは 3 ペイン。右ペイン（作成フォーム・記録項目の詳細）と
 * 「設定」タブ（接続先・収集グループの編集）は同時に描かれ、どちらにも「名前」欄が
 * あるので、ロケータは `records-page.ts` の helper（test id）で必ずスコープする。
 */
import { expect, test, type Dialog, type Page } from '@playwright/test';
import {
	detailPane,
	openCreateConnection,
	openCreateGroup,
	openCreateRecord,
	openListTab,
	openSettingsTab,
	recordList,
	recordTree,
	recordTreePane,
	selectTreeNode,
	settingsPane,
	treeNode,
	treeSelection,
	TREE_PANE_LABEL
} from './records-page';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

// このスペックが作る閲覧者アカウント（非 editor の閲覧専用を確かめるため）。
const VIEWER_USERNAME = 'e2e-tags-viewer';
const VIEWER_PASSWORD = 'E2eViewerPass1';

// serial の再試行は同じ DB のまま最初からやり直すので、固定名だと前回の試行が残した
// 同名の接続先で作成が 422 になる。実行（worker の起動）ごとに一意の接尾辞を付ける。
const RUN = Date.now().toString(36);
const CONNECTION_NAME = `E2E-PLC1-${RUN}`;
const GROUP_NAME = `E2Eグループ1-${RUN}`;
const TAG_NAME = `E2Eタグ1-${RUN}`;
// #1179 PR4b: ツリーの絞り込みを確かめる 2 つ目のグループと記録項目（最後に作る）。
const GROUP2_NAME = `E2Eグループ2-${RUN}`;
const TAG2_NAME = `E2Eタグ2-${RUN}`;

/**
 * 接続先の行の直後に出る「収集グループ未登録」。ツリー全体で探すと、serial の
 * 再試行で前の試行が残した空の接続先の分まで当たるので、今回の接続先の行の隣に絞る
 * （BantoTree の `emptyChildren` は接続先の treeitem の次の兄弟要素）。
 */
function emptyGroupsOf(page: Page, connectionName: string) {
	return treeNode(page, connectionName)
		.locator('xpath=following-sibling::*[1]')
		.getByText('収集グループ未登録');
}

async function login(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード').fill(password);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

/** 狭幅: 帯のトグルでツリーのドロワーを開き、行を選ぶと閉じる。 */
async function selectTreeNodeViaDrawer(page: Page, name: string): Promise<void> {
	const toggle = page.locator(`button[aria-label="${TREE_PANE_LABEL}"]`);
	const treePane = page.locator(`aside[aria-label="${TREE_PANE_LABEL}"]`);
	await expect(treePane).toBeHidden();
	await expect(toggle).toHaveAttribute('aria-expanded', 'false');
	await toggle.click();
	await expect(treePane).toBeVisible();
	// 狭幅は選択がフォーカスに付いてこない（クリック・Enter で選ぶ）。
	await treeNode(page, name).click();
	await expect(treePane).toBeHidden();
	await expect(toggle).toHaveAttribute('aria-expanded', 'false');
}

test.describe.serial('chronogazer 「記録」ページ（#383 段階2a / R1-B）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await login(page, ADMIN_USERNAME, ADMIN_PASSWORD);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. /tags を開くと見出しとツリーが出て、「接続先を追加」でプロトコルごとの欄が出る', async () => {
		await page.goto('/tags');
		await expect(page.getByRole('heading', { level: 2, name: '記録' })).toBeVisible();
		await expect(treeNode(page, 'すべて')).toHaveAttribute('aria-selected', 'true');
		// 「すべて」には「設定」タブが無い（タブ列ごと出さない）。
		await expect(page.getByRole('tab', { name: '設定' })).toHaveCount(0);
		// 記録項目の追加は収集グループを選んでいるときだけ。
		await expect(page.getByRole('button', { name: '記録項目を追加' })).toHaveCount(0);
		await expect(page.getByTestId('record-add-hint')).toBeVisible();
		const form = await openCreateConnection(page);
		await form.getByLabel('プロトコル').selectOption('slmp');
		await expect(form.getByTestId('driver-field-cpu_type')).toHaveCount(1);
		await expect(form.getByTestId('driver-field-unit_id')).toHaveCount(0);
		await form.getByLabel('プロトコル').selectOption('modbus-tcp');
		await expect(form.getByTestId('driver-field-unit_id')).toHaveCount(1);
		await expect(form.getByTestId('driver-field-cpu_type')).toHaveCount(0);
	});

	test('2. 接続先を作成するとツリーに出て、作った接続先が選ばれる', async () => {
		const form = detailPane(page);
		await form.getByLabel('名前').fill(CONNECTION_NAME);
		await form.getByLabel('ホスト').fill('192.168.11.200');
		let releasePost: () => void = () => {};
		const pendingPost = new Promise<void>((resolve) => {
			releasePost = resolve;
		});
		await page.route('**/api/plc-connections', async (route) => {
			if (route.request().method() === 'POST') await pendingPost;
			await route.continue();
		});
		try {
			await form.getByRole('button', { name: '作成' }).click();
			await expect(form.getByTestId('driver-field-host').locator('input')).toBeDisabled();
			await expect(form.getByTestId('driver-field-port').locator('input')).toBeDisabled();
		} finally {
			releasePost();
			// unroute の第 2 引数はハンドラで { behavior } を渡しても外れない。この試験の route は 1 つだけなので
			// unrouteAll で外し、処理中の呼び出しも待つ。
			await page.unrouteAll({ behavior: 'wait' });
		}
		// 作成に成功したら右ペインは閉じ、作った接続先をツリーで選ぶ。
		await expect(detailPane(page)).toHaveCount(0);
		await expect(treeNode(page, CONNECTION_NAME)).toHaveAttribute('aria-selected', 'true');
		await expect(treeNode(page, CONNECTION_NAME)).toHaveAccessibleName(
			`${CONNECTION_NAME} PLC 直結`
		);
		await expect(page).toHaveURL(/\/tags\?target=connection:\d+$/);
		// 収集グループが 0 件の接続先は、その下に「収集グループ未登録」。
		await expect(emptyGroupsOf(page, CONNECTION_NAME)).toBeVisible();
	});

	test('3. 同名の接続先は作れず、「既に使用されています」が名前欄に人間可読で出る', async () => {
		const form = await openCreateConnection(page);
		await form.getByLabel('名前').fill(CONNECTION_NAME);
		await form.getByLabel('ホスト').fill('192.168.11.201');
		await form.getByRole('button', { name: '作成' }).click();
		await expect(form.getByText('既に使用されています')).toBeVisible();
		// 重複作成は成立していない - ツリーの行は増えない。
		await expect(treeNode(page, CONNECTION_NAME)).toHaveCount(1);
		// 入力が残っているので、閉じるときに確かめられる（ここでは破棄する）。
		page.once('dialog', (dialog) => void dialog.accept());
		await form.getByRole('button', { name: '閉じる' }).click();
		await expect(detailPane(page)).toHaveCount(0);
	});

	test('3a. 保存待ちのドライバ欄は編集できず、非既定設定が保存される', async () => {
		await openSettingsTab(page);
		const detail = settingsPane(page);
		await expect(
			detail.getByRole('heading', { level: 3, name: `${CONNECTION_NAME} を編集` })
		).toBeVisible();
		// 以前の一覧の「動作」列の代わり。
		await expect(detail.getByTestId('record-connection-mode')).toContainText('動作:');
		await detail.getByLabel('ホスト').fill('192.168.11.202');
		await detail.getByLabel('ポート').fill('1502');
		await detail.getByLabel('ユニットID').fill('7');
		let releasePut: () => void = () => {};
		const pendingPut = new Promise<void>((resolve) => {
			releasePut = resolve;
		});
		await page.route('**/api/plc-connections/*', async (route) => {
			if (route.request().method() === 'PUT') await pendingPut;
			await route.continue();
		});
		const putDone = page.waitForResponse(
			(res) => res.request().method() === 'PUT' && /\/api\/plc-connections\/\d+$/.test(res.url())
		);
		try {
			await detail.getByRole('button', { name: '保存' }).click();
			await expect(detail.getByTestId('driver-field-host').locator('input')).toBeDisabled();
			await expect(detail.getByTestId('driver-field-port').locator('input')).toBeDisabled();
		} finally {
			releasePut();
			// unroute の第 2 引数はハンドラで { behavior } を渡しても外れない。この試験の route は 1 つだけなので
			// unrouteAll で外し、処理中の呼び出しも待つ。
			await page.unrouteAll({ behavior: 'wait' });
		}
		// 保存の PUT の完了を待ってから次へ進む（待たずに 3b の reload へ進むと PUT が
		// ERR_ABORTED になり、保存前の値が返る）。
		expect((await putDone).ok()).toBe(true);
		await expect(detail.getByLabel('ホスト')).toHaveValue('192.168.11.202');
		await expect(detail.getByLabel('ポート')).toHaveValue('1502');
		await expect(detail.getByLabel('ユニットID')).toHaveValue('7');
		await expect(page).toHaveURL(/\/tags\?target=connection:\d+&tab=settings$/);
	});

	test('3b. catalog が接続先の「設定」より後に届いても保存済み設定を復元する', async () => {
		let releaseCatalog: () => void = () => {};
		const pendingCatalog = new Promise<void>((resolve) => {
			releaseCatalog = resolve;
		});
		await page.route('**/api/drivers', async (route) => {
			await pendingCatalog;
			await route.continue();
		});
		try {
			// URL が対象と「設定」タブを持つので、再読み込みで同じ画面に戻る。
			await page.reload({ waitUntil: 'domcontentloaded' });
			const detail = settingsPane(page);
			await expect(
				detail.getByRole('heading', { level: 3, name: `${CONNECTION_NAME} を編集` })
			).toBeVisible();
			releaseCatalog();
			await expect(detail.getByLabel('ホスト')).toHaveValue('192.168.11.202');
			await expect(detail.getByLabel('ポート')).toHaveValue('1502');
			await expect(detail.getByLabel('ユニットID')).toHaveValue('7');
		} finally {
			releaseCatalog();
			// unroute の第 2 引数はハンドラで { behavior } を渡しても外れない。この試験の route は 1 つだけなので
			// unrouteAll で外し、処理中の呼び出しも待つ。
			await page.unrouteAll({ behavior: 'wait' });
		}
	});

	test('3c. catalog 取得失敗後の再試行でも保存済み設定を復元する', async () => {
		let failOnce = true;
		await page.route('**/api/drivers', async (route) => {
			if (failOnce) {
				failOnce = false;
				await route.fulfill({ status: 503, contentType: 'application/json', body: '{}' });
			} else {
				await route.continue();
			}
		});
		try {
			await page.reload({ waitUntil: 'domcontentloaded' });
			const detail = settingsPane(page);
			await expect(detail.getByText('ドライバ一覧を読み込めませんでした')).toBeVisible();
			await detail.getByRole('button', { name: '再試行' }).click();
			await expect(detail.getByLabel('ホスト')).toHaveValue('192.168.11.202');
			await expect(detail.getByLabel('ポート')).toHaveValue('1502');
			await expect(detail.getByLabel('ユニットID')).toHaveValue('7');
		} finally {
			await page.unroute('**/api/drivers');
		}
	});

	test('4. 収集グループを作成するとツリーに出る（選択中の接続先が初期値）', async () => {
		const form = await openCreateGroup(page);
		await expect(form.getByLabel('接続').locator('option:checked')).toHaveText(
			`${CONNECTION_NAME}（modbus-tcp）`
		);
		await form.getByLabel('名前').fill(GROUP_NAME);
		await form.getByRole('button', { name: '作成' }).click();
		await expect(detailPane(page)).toHaveCount(0);
		await expect(treeNode(page, GROUP_NAME)).toHaveAttribute('aria-selected', 'true');
		await expect(treeNode(page, GROUP_NAME)).toHaveAccessibleName(`${GROUP_NAME} (0) 1s`);
		await expect(emptyGroupsOf(page, CONNECTION_NAME)).toHaveCount(0);
		await expect(page).toHaveURL(/\/tags\?target=group:\d+$/);
	});

	test('5. 記録項目を作成すると一覧に出る（選択中の収集グループが初期値）', async () => {
		await expect(page.getByTestId('record-add-hint')).toHaveCount(0);
		const form = await openCreateRecord(page);
		await expect(form.getByLabel('収集グループ').locator('option:checked')).toHaveText(GROUP_NAME);
		await form.getByLabel('名前').fill(TAG_NAME);
		await form.getByLabel('デバイスアドレス').fill('40001');
		await form.getByRole('button', { name: '作成' }).click();
		// 保存に成功したら右ペインを閉じ、一覧を読み直す。
		await expect(detailPane(page)).toHaveCount(0);
		await expect(recordList(page).getByText(TAG_NAME)).toBeVisible();
		await expect(treeNode(page, GROUP_NAME)).toHaveAccessibleName(`${GROUP_NAME} (1) 1s`);
	});

	test('5b. しきい値（記録計の設定）を別に保存でき、再読み込みしても残る。大小関係が崩れると理由が欄に出る。タグの保存はしきい値を消さない（#525 / #532）', async () => {
		await recordList(page).getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		const detail = detailPane(page);
		await expect(detail.getByRole('heading', { level: 3, name: TAG_NAME })).toBeVisible();
		const thresholds = detail.locator('div.thresholds-pane');
		await expect(
			thresholds.getByRole('heading', { level: 4, name: 'しきい値（記録計の設定）' })
		).toBeVisible();
		// 新しいタグは設定なし（既定）。
		await expect(thresholds.getByLabel('しきい値 H（上限）', { exact: true })).toHaveValue('');

		// 大小関係が崩れた組（L > H）はサーバーが拒否し、欄の下に理由が出る。
		await thresholds.getByLabel('しきい値 L（下限）', { exact: true }).fill('50');
		await thresholds.getByLabel('しきい値 H（上限）', { exact: true }).fill('10');
		await thresholds.getByRole('button', { name: 'しきい値を保存', exact: true }).click();
		// `validate_threshold_order`（apps/chronogazer/core/src/tag_thresholds.rs）の文言が H 欄に出る。
		await expect(thresholds.getByText('thresholdL 以上の値にしてください')).toBeVisible();

		// 正しい組で保存する。案内は「収集を再起動」で反映すること。
		await thresholds.getByLabel('しきい値 LL（下下限）', { exact: true }).fill('5');
		await thresholds.getByLabel('しきい値 L（下限）', { exact: true }).fill('10');
		await thresholds.getByLabel('しきい値 H（上限）', { exact: true }).fill('80');
		await thresholds.getByLabel('しきい値 HH（上上限）', { exact: true }).fill('90');
		await thresholds.getByRole('button', { name: 'しきい値を保存', exact: true }).click();
		await expect(page.getByText('しきい値を保存しました')).toBeVisible();

		// タグの定義だけを保存しても、しきい値は消えない（別の保存）。
		await detail.getByLabel('単位', { exact: true }).fill('kPa');
		await detail.getByRole('button', { name: '保存', exact: true }).click();
		await expect(page.getByText('更新しました')).toBeVisible();

		// 再読み込みしても、行を選び直しても値が残る。
		await page.reload();
		await recordList(page).getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		await expect(thresholds.getByLabel('しきい値 LL（下下限）', { exact: true })).toHaveValue('5');
		await expect(thresholds.getByLabel('しきい値 L（下限）', { exact: true })).toHaveValue('10');
		await expect(thresholds.getByLabel('しきい値 H（上限）', { exact: true })).toHaveValue('80');
		await expect(thresholds.getByLabel('しきい値 HH（上上限）', { exact: true })).toHaveValue('90');
		await expect(detail.getByLabel('単位', { exact: true })).toHaveValue('kPa');
	});

	test('5c. 別の画面が先に保存したしきい値を古い版で保存すると、再読み込みを促す案内が出て上書きされない（#525 / #532）', async ({
		browser
	}) => {
		const thresholds = detailPane(page).locator('div.thresholds-pane');
		// 1 枚目の画面でしきい値のフォームを開いたまま（この時点の版を持っている）。
		await expect(thresholds.getByLabel('しきい値 H（上限）', { exact: true })).toHaveValue('80');

		// 2 枚目の画面が先に H を 85 へ保存する。
		const other = await browser.newPage();
		try {
			await login(other, ADMIN_USERNAME, ADMIN_PASSWORD);
			await other.goto('/tags');
			await recordList(other).getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
			const otherThresholds = detailPane(other).locator('div.thresholds-pane');
			await expect(otherThresholds.getByLabel('しきい値 H（上限）', { exact: true })).toHaveValue(
				'80'
			);
			await otherThresholds.getByLabel('しきい値 H（上限）', { exact: true }).fill('85');
			await otherThresholds.getByRole('button', { name: 'しきい値を保存', exact: true }).click();
			await expect(other.getByText('しきい値を保存しました')).toBeVisible();
		} finally {
			await other.close();
		}

		// 1 枚目が古い版のまま H を 88 にして保存 -> 拒否される。
		await thresholds.getByLabel('しきい値 H（上限）', { exact: true }).fill('88');
		await thresholds.getByRole('button', { name: 'しきい値を保存', exact: true }).click();
		await expect(page.getByText('先に更新しました')).toBeVisible();

		// 先に保存された 85 が残っている（88 で上書きされていない）。
		await page.reload();
		await recordList(page).getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		await expect(thresholds.getByLabel('しきい値 H（上限）', { exact: true })).toHaveValue('85');
	});

	test('6. スケーリングを部分指定（生値下限だけ）で作成しようとすると、理由が画面に見える', async () => {
		// 右ペインの詳細を開いたままでも、未保存が無ければ確かめずに作成フォームへ替わる。
		const form = await openCreateRecord(page);
		const PARTIAL_SCALING_TAG_NAME = 'E2Eスケーリング欠落';
		await form.getByLabel('名前').fill(PARTIAL_SCALING_TAG_NAME);
		await form.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		await form.getByLabel('デバイスアドレス').fill('40011');
		await form.getByLabel('スケーリング: 生値 下限').fill('0');
		await form.getByRole('button', { name: '作成' }).click();
		// `Scaling::from_parts()`（crates/banto-tags/src/scaling.rs）が返す
		// 理由がそのまま出る - `scaling`というフィールドはフォームに無いため、
		// 4つの生値/工学値項目すべてに同じメッセージが出る（`.first()`で
		// strict mode 違反を避ける）。
		await expect(
			form
				.getByText('raw_lo/raw_hi/eng_lo/eng_hi は全て指定するか、全て未指定にしてください')
				.first()
		).toBeVisible();
		// 検証エラーで弾かれ、作成は成立していない。
		await expect(recordList(page).getByText(PARTIAL_SCALING_TAG_NAME)).toHaveCount(0);
	});

	test('6b. Modbus 接続の下に MELSEC 表記のアドレスは保存できず、理由がアドレス欄に見える（#414）', async () => {
		const form = detailPane(page);
		const MELSEC_TAG_NAME = 'E2E-MELSEC表記';
		await form.getByLabel('名前').fill(MELSEC_TAG_NAME);
		await form.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		await form.getByLabel('デバイスアドレス').fill('D3000');
		// 6 の部分スケーリングが残っていると、そちらの理由と混ざる。
		await form.getByLabel('スケーリング: 生値 下限').fill('');
		await form.getByRole('button', { name: '作成' }).click();
		// 登録簿モードが返すドライバの検査理由。
		await expect(form.getByText(/Modbus の参照番号/)).toBeVisible();
		// 保存は成立していない。
		await expect(recordList(page).getByText(MELSEC_TAG_NAME)).toHaveCount(0);
	});

	test('7. 収集グループが残っている接続先の削除は具体的な理由で拒否され、データは消えない', async () => {
		// 右ペインの作成フォームに入力が残っているので、対象を変えると確かめる（破棄する）。
		page.once('dialog', (dialog) => void dialog.accept());
		await selectTreeNode(page, CONNECTION_NAME);
		await expect(detailPane(page)).toHaveCount(0);
		await openSettingsTab(page);
		const detail = settingsPane(page);
		await expect(
			detail.getByRole('heading', { level: 3, name: `${CONNECTION_NAME} を編集` })
		).toBeVisible();
		page.once('dialog', (dialog) => void dialog.accept());
		await detail.getByRole('button', { name: '削除' }).click();
		// 修正前は `ProviderError.message` が一律 "validation failed" になり
		// この理由が消えていた（#391レビュー C）。
		await expect(
			page.getByText('この接続を使用している収集グループが1件あるため削除できません')
		).toBeVisible();
		// 削除は成立していない - ツリーからも消えていない。
		await expect(treeNode(page, CONNECTION_NAME)).toBeVisible();
	});

	test('8. 閲覧者アカウントを作成する（次のテストの前提）', async () => {
		await page.goto('/users');
		// 入力欄は「新規作成」セクションに限定して取る（一覧のグリッドに
		// 同名の列見出しがあり strict mode 違反になるため）。
		const createForm = page.locator('section.create');
		await createForm.getByLabel('ユーザー名').fill(VIEWER_USERNAME);
		await createForm.getByLabel('パスワード（8文字以上）').fill(VIEWER_PASSWORD);
		await createForm.getByLabel('表示名').fill('E2Eタグ閲覧者');
		await createForm.getByLabel('ロール').selectOption('viewer');
		await createForm.getByRole('button', { name: '作成' }).click();
		await expect(page.locator('section.list').getByText(VIEWER_USERNAME).first()).toBeVisible();
	});

	test('9. viewer は /tags を閲覧のみでき、追加の導線・「設定」タブ・右ペイン・削除ボタンは出ない', async ({
		browser
	}) => {
		const viewerPage = await browser.newPage();
		try {
			await login(viewerPage, VIEWER_USERNAME, VIEWER_PASSWORD);
			await viewerPage.goto('/tags');
			await expect(viewerPage.getByRole('heading', { level: 2, name: '記録' })).toBeVisible();
			// ツリーと一覧（読み取り）はそのまま見える。
			await expect(treeNode(viewerPage, CONNECTION_NAME)).toBeVisible();
			await expect(treeNode(viewerPage, GROUP_NAME)).toBeVisible();
			await expect(recordList(viewerPage).getByText(TAG_NAME)).toBeVisible();
			await selectTreeNode(viewerPage, GROUP_NAME);
			await expect(recordList(viewerPage).getByText(TAG_NAME)).toBeVisible();
			// 追加・編集・削除の導線は一切出ない。
			await expect(viewerPage.getByRole('button', { name: '接続先を追加' })).toHaveCount(0);
			await expect(viewerPage.getByRole('button', { name: '収集グループを追加' })).toHaveCount(0);
			await expect(viewerPage.getByRole('button', { name: '記録項目を追加' })).toHaveCount(0);
			await expect(viewerPage.getByRole('button', { name: 'Hub から追加' })).toHaveCount(0);
			await expect(viewerPage.getByRole('tab', { name: '設定' })).toHaveCount(0);
			await recordList(viewerPage).getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
			await expect(detailPane(viewerPage)).toHaveCount(0);
			await expect(viewerPage.getByRole('button', { name: '削除' })).toHaveCount(0);
			// 「設定」タブの URL を直接開いても「記録項目」へ倒れる。
			const url = new URL(viewerPage.url());
			url.searchParams.set('tab', 'settings');
			await viewerPage.goto(`${url.pathname}${decodeURIComponent(url.search)}`);
			await expect(viewerPage).toHaveURL(/\/tags\?target=group:\d+$/);
			await expect(viewerPage.getByRole('tab', { name: '設定' })).toHaveCount(0);
		} finally {
			await viewerPage.close();
		}
	});

	test('10. 削除の応答待ちの間は編集フォームが無効化され、保存を試みてもPUTは送信されない。拒否後は編集操作が再び有効に戻る', async () => {
		// #394レビューP2: `deletingConnection`が真の間、以前は編集フォーム
		// （`BantoForm`の`submitting`）が素通しで、削除の応答待ちの間に同じ
		// PLC接続へ`PUT`を送れてしまっていた。CONNECTION_NAMEはGROUP_NAMEが
		// 参照しているため、このDELETEはサーバー側の検証で拒否される
		// （#391レビューCで固定した既存の拒否経路 - test 7と同じ理由）。
		await page.goto('/tags');

		// `DELETE /api/plc-connections/:id`だけを遅延させ、無効化ウィンドウを
		// 作る（作法は`e2e/tests-banto-hub/banto-hub-tags-busy.spec.ts`の
		// `page.route`ゲートに倣う）。`PUT`はカウントするだけで素通しする -
		// 「保存を試みても実際にはPUTが飛んでいない」ことをネットワークレベルで
		// 確認するため（ボタンのdisabled表示だけでは、もし保存ハンドラの
		// ガードが抜けていても見た目上は気づけない）。
		let releaseDelete: (() => void) | undefined;
		const deleteGate = new Promise<void>((resolve) => {
			releaseDelete = resolve;
		});
		let putCount = 0;
		await page.route('**/api/plc-connections/*', async (route) => {
			const req = route.request();
			if (req.method() === 'DELETE') {
				await deleteGate;
				await route.continue();
				return;
			}
			if (req.method() === 'PUT') {
				putCount += 1;
			}
			await route.continue();
		});

		try {
			await selectTreeNode(page, CONNECTION_NAME);
			await openSettingsTab(page);
			const detail = settingsPane(page);
			await expect(
				detail.getByRole('heading', { level: 3, name: `${CONNECTION_NAME} を編集` })
			).toBeVisible();

			const nameInput = detail.getByLabel('名前');
			const saveButton = detail.getByRole('button', { name: '保存' });
			const deleteButton = detail.getByRole('button', { name: '削除' });

			page.once('dialog', (dialog) => void dialog.accept());
			await deleteButton.click();

			// DELETEの応答待ちの間: 入力欄・保存ボタン・削除ボタンが無効。
			await expect(nameInput).toBeDisabled();
			await expect(saveButton).toBeDisabled();
			await expect(deleteButton).toBeDisabled();

			// 保存を試みてもPUTは送信されない（ネイティブのdisabled要素は
			// forceクリックしてもクリックイベントを発火しない - ブラウザの
			// 標準動作。それでも実際にネットワークへ出ていないことを
			// `putCount`で確認する）。
			await saveButton.click({ force: true, timeout: 2000 }).catch(() => {});
			expect(putCount).toBe(0);

			// ゲートを解放し、拒否応答を受け取る。
			const deleteResponse = page.waitForResponse(
				(res) =>
					res.request().method() === 'DELETE' && /\/api\/plc-connections\/\d+$/.test(res.url())
			);
			releaseDelete?.();
			await deleteResponse;

			// 拒否理由が具体的に出て、データは消えていない（#391レビューC）。
			await expect(
				page.getByText('この接続を使用している収集グループが1件あるため削除できません')
			).toBeVisible();
			await expect(treeNode(page, CONNECTION_NAME)).toBeVisible();

			// 拒否後は編集操作が再び有効に戻る。
			await expect(nameInput).toBeEnabled();
			await expect(saveButton).toBeEnabled();
			await expect(deleteButton).toBeEnabled();

			expect(putCount).toBe(0);
		} finally {
			await page.unroute('**/api/plc-connections/*');
		}
	});

	test('11. 一覧の取得に失敗しても「0件」に潰れず、誤った案内を出さない（#394レビュー P1-3）', async () => {
		const treePane = recordTreePane(page);

		// `GET /api/plc-connections` だけを落とす（POST や
		// `/api/plc-connections/{id}` には触れない）。
		await page.route('**/api/plc-connections', async (route) => {
			if (route.request().method() === 'GET') {
				await route.abort();
				return;
			}
			await route.continue();
		});
		try {
			await page.goto('/tags');

			// 失敗が分かる表示と、再試行の導線。`tagRegistryAdmin.ts` の
			// `httpRequest` はネットワーク層の例外を
			// `ProviderError({ kind: 'other', message: 'サーバーに接続できません' })`
			// に変換するので、その文言がそのまま添えられる。
			await expect(
				treePane.getByText('接続先の一覧を読み込めませんでした', { exact: false })
			).toBeVisible();
			await expect(treePane.getByRole('button', { name: '再試行' }).first()).toBeVisible();

			// 0件として描かない: ツリー（「すべて」だけの木）も出さない。
			await expect(recordTree(page)).toHaveCount(0);

			// **誤った指示を出さない**（修正前はここが出て、利用者を重複登録へ
			// 誘導していた）。候補が無いように見える `<select>` も出さない。
			const form = await openCreateGroup(page);
			await expect(page.getByText('先に接続先を1件以上作成してください')).toHaveCount(0);
			await expect(form.getByLabel('接続')).toHaveCount(0);
			await form.getByRole('button', { name: '閉じる' }).click();
			await expect(detailPane(page)).toHaveCount(0);
		} finally {
			await page.unroute('**/api/plc-connections');
		}

		// route を外して再試行すれば復旧する（失敗は終端状態ではない）。
		await treePane.getByRole('button', { name: '再試行' }).first().click();
		await expect(treeNode(page, CONNECTION_NAME)).toBeVisible();
		await expect(
			treePane.getByText('接続先の一覧を読み込めませんでした', { exact: false })
		).toHaveCount(0);
	});

	test('12. ツリーで選ぶと記録項目の一覧が絞られる（#1179 PR4b）', async () => {
		// 2 つ目のグループと記録項目を同じ接続先の下に作る。
		await selectTreeNode(page, CONNECTION_NAME);
		const groupForm = await openCreateGroup(page);
		await groupForm.getByLabel('名前').fill(GROUP2_NAME);
		await groupForm.getByRole('button', { name: '作成' }).click();
		await expect(treeNode(page, GROUP2_NAME)).toHaveAttribute('aria-selected', 'true');
		const tagForm = await openCreateRecord(page);
		await expect(tagForm.getByLabel('収集グループ').locator('option:checked')).toHaveText(
			GROUP2_NAME
		);
		await tagForm.getByLabel('名前').fill(TAG2_NAME);
		await tagForm.getByLabel('デバイスアドレス').fill('40002');
		await tagForm.getByRole('button', { name: '作成' }).click();
		await expect(detailPane(page)).toHaveCount(0);

		// グループ = そのグループの記録項目だけ。
		const list = recordList(page);
		await expect(list.getByText(TAG2_NAME)).toBeVisible();
		await expect(list.getByText(TAG_NAME, { exact: true })).toHaveCount(0);
		await selectTreeNode(page, GROUP_NAME);
		await expect(list.getByText(TAG_NAME, { exact: true })).toBeVisible();
		await expect(list.getByText(TAG2_NAME)).toHaveCount(0);
		// 接続先 = 配下のグループの記録項目。すべて = 全件。
		await selectTreeNode(page, CONNECTION_NAME);
		await expect(list.getByText(TAG_NAME, { exact: true })).toBeVisible();
		await expect(list.getByText(TAG2_NAME)).toBeVisible();
		await selectTreeNode(page, 'すべて');
		await expect(list.getByText(TAG_NAME, { exact: true })).toBeVisible();
		await expect(list.getByText(TAG2_NAME)).toBeVisible();
		await expect(page).toHaveURL(/\/tags$/);
	});

	test('13. URL を開き直す・戻ると、同じ対象とタブに戻る（#1179 PR4b）', async () => {
		await selectTreeNode(page, GROUP2_NAME);
		await openSettingsTab(page);
		await expect(page).toHaveURL(/\/tags\?target=group:\d+&tab=settings$/);
		const url = page.url();

		await page.goto('/tags');
		await expect(treeNode(page, 'すべて')).toHaveAttribute('aria-selected', 'true');
		await page.goto(url);
		await expect(treeNode(page, GROUP2_NAME)).toHaveAttribute('aria-selected', 'true');
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');
		await expect(
			settingsPane(page).getByRole('heading', { level: 3, name: `${GROUP2_NAME} を編集` })
		).toBeVisible();
		await expect(treeSelection(page)).toHaveText(GROUP2_NAME);

		// タブを替えてから戻ると「設定」に戻る。
		await openListTab(page);
		await expect(page).toHaveURL(/\/tags\?target=group:\d+$/);
		await page.goBack();
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');
		await expect(page).toHaveURL(url);

		// 一覧に無い id は「すべて」・「記録項目」へ倒し、URL も直す。
		await page.goto('/tags?target=group:999999&tab=settings');
		await expect(treeNode(page, 'すべて')).toHaveAttribute('aria-selected', 'true');
		await expect(page).toHaveURL(/\/tags$/);
		await expect(page.getByText('指定された接続先・収集グループが見つからないため')).toBeVisible();
	});

	test('14. 未保存のままタブ・対象を変えると確認が出て、キャンセルなら元のまま（#1179 PR4b）', async () => {
		await selectTreeNode(page, GROUP2_NAME);
		await openSettingsTab(page);
		const name = settingsPane(page).getByLabel('名前');
		await name.fill(`${GROUP2_NAME}-編集中`);

		// タブを変える -> キャンセル -> 「設定」のまま、入力も残る。
		let asked = '';
		page.once('dialog', (dialog) => {
			asked = dialog.message();
			void dialog.dismiss();
		});
		await page.getByRole('tab', { name: '記録項目' }).click();
		await expect.poll(() => asked).toContain('保存していない変更があります');
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');
		await expect(name).toHaveValue(`${GROUP2_NAME}-編集中`);

		// ツリーで別の対象を選ぶ -> キャンセル -> 選択も入力も元のまま。
		page.once('dialog', (dialog) => void dialog.dismiss());
		await treeNode(page, GROUP_NAME).click();
		await expect(treeNode(page, GROUP2_NAME)).toHaveAttribute('aria-selected', 'true');
		await expect(name).toHaveValue(`${GROUP2_NAME}-編集中`);
		await expect(page).toHaveURL(/\/tags\?target=group:\d+&tab=settings$/);

		// 戻る操作でも確かめる（キャンセルなら残る）。
		page.once('dialog', (dialog) => void dialog.dismiss());
		await page.goBack();
		await expect(name).toHaveValue(`${GROUP2_NAME}-編集中`);
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');

		// 破棄を選ぶと切り替わり、保存済みの値に戻る。
		page.once('dialog', (dialog) => void dialog.accept());
		await page.getByRole('tab', { name: '記録項目' }).click();
		await expect(page.getByRole('tab', { name: '記録項目' })).toHaveAttribute(
			'aria-selected',
			'true'
		);
		await openSettingsTab(page);
		await expect(settingsPane(page).getByLabel('名前')).toHaveValue(GROUP2_NAME);
	});

	test('15. 900px 以下では左がドロワーになり、閉じていても選択中の名前が残る（#1179 PR4b）', async () => {
		await page.setViewportSize({ width: 800, height: 900 });
		try {
			await page.goto('/tags');
			await selectTreeNodeViaDrawer(page, GROUP_NAME);
			await expect(treeSelection(page)).toHaveText(GROUP_NAME);
			await expect(recordList(page).getByText(TAG_NAME, { exact: true })).toBeVisible();
			await expect(recordList(page).getByText(TAG2_NAME)).toHaveCount(0);
		} finally {
			await page.setViewportSize({ width: 1280, height: 720 });
		}
	});

	test('16. 版の衝突の後に同じ行を選び直すと、最新の版がフォームに出て保存できる（#1179 PR4b レビュー）', async () => {
		await page.goto('/tags');
		await recordList(page).getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		const detail = detailPane(page);
		await expect(detail.getByRole('heading', { level: 3, name: TAG_NAME })).toBeVisible();

		// 別経路（API）が先に同じ記録項目の単位を更新する。
		const login = await page.request.post('/api/auth/login', {
			headers: { 'X-Banto-Client': 'banto' },
			data: { username: ADMIN_USERNAME, password: ADMIN_PASSWORD }
		});
		const { token } = (await login.json()) as { token: string };
		const headers = { 'X-Banto-Client': 'banto', Authorization: `Bearer ${token}` };
		const list = await page.request.get('/api/tags', { headers });
		const rows = (await list.json()) as Array<Record<string, unknown>>;
		const row = rows.find((r) => r.name === TAG_NAME);
		expect(row).toBeTruthy();
		const { id, revision, ...rest } = row as Record<string, unknown>;
		const put = await page.request.put(`/api/tags/${String(id)}`, {
			headers,
			data: { ...rest, unit: 'api', expectedRevision: revision }
		});
		expect(put.ok()).toBe(true);

		// 古い版のまま画面で保存 -> 衝突の案内。入力は残る。
		const unit = detail.getByLabel('単位', { exact: true });
		await unit.fill('ui');
		await detail.getByRole('button', { name: '保存', exact: true }).click();
		await expect(page.getByText('先に更新しました')).toBeVisible();
		await expect(unit).toHaveValue('ui');

		// 同じ行をもう一度選ぶと、未保存の入力を捨てるかを確かめる。キャンセルなら入力が残る。
		const recordRow = recordList(page).getByRole('gridcell', { name: TAG_NAME, exact: true });
		const dismissed: string[] = [];
		page.once('dialog', (dialog) => {
			dismissed.push(dialog.message());
			void dialog.dismiss();
		});
		await recordRow.click();
		await expect.poll(() => dismissed.length).toBe(1);
		await expect(unit).toHaveValue('ui');

		// 承認すると、最新の値（API が書いた単位）で作り直される。
		const accepted: string[] = [];
		page.once('dialog', (dialog) => {
			accepted.push(dialog.message());
			void dialog.accept();
		});
		await recordRow.click();
		await expect.poll(() => accepted.length).toBe(1);
		await expect(unit).toHaveValue('api');

		// 最新の版で保存できる。
		await unit.fill('ui2');
		await detail.getByRole('button', { name: '保存', exact: true }).click();
		await expect(page.getByText('更新しました', { exact: true })).toBeVisible();
		await expect(unit).toHaveValue('ui2');
	});

	test('17. 削除の成功は、無関係な右ペインの作成フォームと入力を捨てない（#1179 PR4b レビュー）', async () => {
		const DELETE_GROUP = 'E2E削除用グループ';
		const DRAFT_NAME = 'E2E下書き接続先';
		await page.goto('/tags');
		await selectTreeNode(page, CONNECTION_NAME);
		const groupForm = await openCreateGroup(page);
		await groupForm.getByLabel('名前').fill(DELETE_GROUP);
		await groupForm.getByRole('button', { name: '作成' }).click();
		await expect(treeNode(page, DELETE_GROUP)).toHaveAttribute('aria-selected', 'true');
		await expect(detailPane(page)).toHaveCount(0);

		// 配下の無いグループの「設定」を開き、右に無関係な作成フォームを入力途中にする。
		await openSettingsTab(page);
		const draft = await openCreateConnection(page);
		await draft.getByLabel('名前').fill(DRAFT_NAME);

		// 中央で削除を承認する。
		page.once('dialog', (dialog) => void dialog.accept());
		await settingsPane(page).getByRole('button', { name: '削除' }).click();
		await expect(treeNode(page, DELETE_GROUP)).toHaveCount(0);

		// 削除は成功し、右の追加フォームと入力は残っている。
		await expect(
			detailPane(page).getByRole('heading', { level: 3, name: '接続先を追加' })
		).toBeVisible();
		await expect(detailPane(page).getByLabel('名前')).toHaveValue(DRAFT_NAME);
	});

	test('18. 未保存のまま戻ると、確認はページの「切り替えますか」の 1 回だけで、キャンセルなら元のまま（#1179 PR4b）', async () => {
		const DRAFT_NAME = 'E2E戻る下書き接続先';
		// 前のテストの下書きが残っていてもよいよう、準備の間の確認は承認する。
		const asked: string[] = [];
		let answer: 'accept' | 'dismiss' = 'accept';
		const onDialog = (dialog: Dialog): void => {
			if (answer === 'dismiss') asked.push(dialog.message());
			void (answer === 'accept' ? dialog.accept() : dialog.dismiss());
		};
		page.on('dialog', onDialog);
		try {
			await page.goto('/tags');
			await selectTreeNode(page, CONNECTION_NAME);
			await expect(page).toHaveURL(/\/tags\?target=connection:\d+$/);
			await selectTreeNode(page, GROUP_NAME);
			await expect(page).toHaveURL(/\/tags\?target=group:\d+$/);
			const groupUrl = page.url();

			const draft = await openCreateConnection(page);
			await draft.getByLabel('名前').fill(DRAFT_NAME);

			// 戻る -> ページの確認だけが 1 回出る -> キャンセルで元のまま。
			answer = 'dismiss';
			await page.goBack();
			await expect.poll(() => asked.length).toBe(1);
			expect(asked[0]).toContain('変更を破棄して切り替えますか');
			await expect(page).toHaveURL(groupUrl);
			await expect(treeNode(page, GROUP_NAME)).toHaveAttribute('aria-selected', 'true');
			await expect(
				detailPane(page).getByRole('heading', { level: 3, name: '接続先を追加' })
			).toBeVisible();
			await expect(detailPane(page).getByLabel('名前')).toHaveValue(DRAFT_NAME);
			// 遅れて二度目の確認が出ないこと。
			await page.waitForTimeout(500);
			expect(asked).toHaveLength(1);
		} finally {
			page.off('dialog', onDialog);
		}
	});
});
