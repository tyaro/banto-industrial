/**
 * タグ設定画面（`/tags`）の E2E 固定（#383 段階2a / R1-B）。
 *
 * 固定したい受入条件:
 * 1. 管理者で `/tags` を開き、PLC接続 → 収集グループ → タグ の順に作成でき、
 *    それぞれ一覧に出ること（FK の依存順どおり、直前に作った行が次の
 *    セクションのプルダウンに現れること込み）。
 * 2. 検証エラーが人間可読で出ること - `banto_tags` の UNIQUE 制約
 *    （`plc_connections.name` は全体で一意、`crates/banto-tags/src/support.rs`
 *    の `map_write_error`）を、同名の PLC接続を2つ作ろうとして踏み、
 *    「既に使用されています」が名前欄のすぐ下に出ることを確認する。
 *    （`address`/`periodMs` は指示書の例だが、このアプリの UI ではどちらも
 *    実際には到達できない - `periodMs` は固定選択肢の `<select>` なので
 *    範囲外の値を選びようがなく、`plc` タグの `address` は banto-tags 側で
 *    非空しか検証しないため形式エラーが存在しない。名前の重複は同じ
 *    「サーバー側検証がフィールドへ人間可読で返る」経路を、実際に踏める
 *    形で固定したもの。）
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
 *
 * ファイル名について: `smoke.spec.ts` が初回セットアップ（管理者アカウント
 * 作成）を実 DOM で行うため、このファイルは辞書順でそれより後でなければ
 * ならない（`playwright.config.ts` は `workers: 1`/`fullyParallel: false` で
 * `testDir` 配下をファイル名の辞書順に実行する - `user-settings-hub.spec.ts`
 * の doc comment にある罠と同じ）。`tags` は `smoke` より後、`user-*` より
 * 前に入る（`s` < `t` < `u`）。
 *
 * 3セクションが同じページに同時に描画されるため、ロケータは必ず
 * `section.registry-section`（0=PLC接続, 1=収集グループ, 2=タグ、画面の
 * 描画順どおり）とその中の `div.create`/`div.list` でスコープする -
 * スコープしないと「名前」ラベルの入力欄が3つとも一致して strict mode
 * 違反になる。
 */
import { expect, test, type Page } from '@playwright/test';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

// このスペックが作る閲覧者アカウント（非 editor の閲覧専用を確かめるため）。
const VIEWER_USERNAME = 'e2e-tags-viewer';
const VIEWER_PASSWORD = 'E2eViewerPass1';

const CONNECTION_NAME = 'E2E-PLC1';
const GROUP_NAME = 'E2Eグループ1';
const TAG_NAME = 'E2Eタグ1';

async function login(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード').fill(password);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

test.describe.serial('chronogazer タグ設定画面（#383 段階2a / R1-B）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await login(page, ADMIN_USERNAME, ADMIN_PASSWORD);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. /tags を開くと見出しと3セクションが出る', async () => {
		await page.goto('/tags');
		await expect(page.getByRole('heading', { level: 2, name: 'タグ設定' })).toBeVisible();
		await expect(page.getByRole('heading', { level: 3, name: 'PLC接続' })).toBeVisible();
		await expect(page.getByRole('heading', { level: 3, name: '収集グループ' })).toBeVisible();
		await expect(page.getByRole('heading', { level: 3, name: 'タグ' })).toBeVisible();
	});

	test('2. PLC接続を作成すると一覧に出る', async () => {
		const section = page.locator('section.registry-section').nth(0);
		const form = section.locator('div.create');
		await form.getByLabel('名前').fill(CONNECTION_NAME);
		await form.getByLabel('ホスト').fill('192.168.11.200');
		await form.getByRole('button', { name: '作成' }).click();
		await expect(section.locator('div.list').getByText(CONNECTION_NAME)).toBeVisible();
	});

	test('3. 同名のPLC接続は作れず、「既に使用されています」が名前欄に人間可読で出る', async () => {
		const section = page.locator('section.registry-section').nth(0);
		const form = section.locator('div.create');
		await form.getByLabel('名前').fill(CONNECTION_NAME);
		await form.getByLabel('ホスト').fill('192.168.11.201');
		await form.getByRole('button', { name: '作成' }).click();
		await expect(form.getByText('既に使用されています')).toBeVisible();
		// 重複作成は成立していない - 一覧の行数は増えない。
		await expect(section.locator('div.list').getByText(CONNECTION_NAME)).toHaveCount(1);
	});

	test('4. 収集グループを作成すると一覧に出る（直前のPLC接続がプルダウンに現れる）', async () => {
		const section = page.locator('section.registry-section').nth(1);
		const form = section.locator('div.create');
		await form.getByLabel('名前').fill(GROUP_NAME);
		await form.getByLabel('PLC接続').selectOption({ label: `${CONNECTION_NAME}（modbus-tcp）` });
		await form.getByRole('button', { name: '作成' }).click();
		await expect(section.locator('div.list').getByText(GROUP_NAME)).toBeVisible();
	});

	test('5. タグを作成すると一覧に出る（直前の収集グループがプルダウンに現れる）', async () => {
		const section = page.locator('section.registry-section').nth(2);
		const form = section.locator('div.create');
		await form.getByLabel('名前').fill(TAG_NAME);
		await form.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		await form.getByLabel('デバイスアドレス').fill('D3000');
		await form.getByRole('button', { name: '作成' }).click();
		await expect(section.locator('div.list').getByText(TAG_NAME)).toBeVisible();
	});

	test('6. スケーリングを部分指定（生値下限だけ）で作成しようとすると、理由が画面に見える', async () => {
		const section = page.locator('section.registry-section').nth(2);
		const form = section.locator('div.create');
		const PARTIAL_SCALING_TAG_NAME = 'E2Eスケーリング欠落';
		await form.getByLabel('名前').fill(PARTIAL_SCALING_TAG_NAME);
		await form.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		await form.getByLabel('デバイスアドレス').fill('D3010');
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
		await expect(section.locator('div.list').getByText(PARTIAL_SCALING_TAG_NAME)).toHaveCount(0);
	});

	test('7. 収集グループが残っているPLC接続の削除は具体的な理由で拒否され、データは消えない', async () => {
		const section = page.locator('section.registry-section').nth(0);
		await section
			.locator('div.list')
			.getByRole('gridcell', { name: CONNECTION_NAME, exact: true })
			.click();
		const detail = section.locator('div.detail');
		await expect(
			detail.getByRole('heading', { level: 4, name: `${CONNECTION_NAME} を編集` })
		).toBeVisible();
		page.once('dialog', (dialog) => void dialog.accept());
		await detail.getByRole('button', { name: '削除' }).click();
		// 修正前は `ProviderError.message` が一律 "validation failed" になり
		// この理由が消えていた（#391レビュー C）。
		await expect(
			page.getByText('この接続を使用している収集グループが1件あるため削除できません')
		).toBeVisible();
		// 削除は成立していない - 一覧からも消えていない。
		await expect(section.locator('div.list').getByText(CONNECTION_NAME)).toBeVisible();
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

	test('9. viewer は /tags を閲覧のみでき、新規作成フォーム・削除ボタンは出ない', async ({
		browser
	}) => {
		const viewerPage = await browser.newPage();
		try {
			await login(viewerPage, VIEWER_USERNAME, VIEWER_PASSWORD);
			await viewerPage.goto('/tags');
			await expect(viewerPage.getByRole('heading', { level: 2, name: 'タグ設定' })).toBeVisible();
			// 一覧（読み取り）はそのまま見える。収集グループ一覧の「PLC接続」列にも
			// 接続名がそのまま出るため、PLC接続名は PLC接続セクション（0番目）の
			// 一覧に絞って探す - 絞らないと2箇所に一致して strict mode 違反になる。
			const plcSection = viewerPage.locator('section.registry-section').nth(0);
			const groupSection = viewerPage.locator('section.registry-section').nth(1);
			const tagSection = viewerPage.locator('section.registry-section').nth(2);
			await expect(plcSection.locator('div.list').getByText(CONNECTION_NAME)).toBeVisible();
			await expect(groupSection.locator('div.list').getByText(GROUP_NAME)).toBeVisible();
			await expect(tagSection.locator('div.list').getByText(TAG_NAME)).toBeVisible();
			// 新規作成・編集・削除の導線は一切出ない。
			await expect(viewerPage.getByRole('heading', { level: 4, name: '新規作成' })).toHaveCount(0);
			await expect(viewerPage.getByRole('button', { name: '削除' })).toHaveCount(0);
		} finally {
			await viewerPage.close();
		}
	});
});
