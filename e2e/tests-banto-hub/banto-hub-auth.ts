/**
 * banto-hub E2E スイート共通の認証ヘルパー（`banto-hub-smoke.spec.ts` /
 * `banto-hub-tags-continuous.spec.ts` の両方から使う）。ファイル名が
 * `banto-hub-*.spec.ts` に一致しないので `banto-hub.playwright.config.ts`
 * の `testMatch` には拾われず、テストファイルとしては実行されない。
 *
 * `@banto/admin-core`（`createHttpAuthProvider`、
 * `apps/banto-hub/src/lib/banto/setup.ts` が既定オプションで配線）の
 * 実装に合わせている:
 * - すべての管理系 REST は `X-Banto-Client: banto` ヘッダー必須
 *   （`banto_server::csrf::require_banto_client_header` -
 *   `apps/banto-hub/core/src/rest.rs` の admin router 全体に layer 済み）。
 * - ログイン成功後のトークンは `sessionStorage['banto.auth.token']`
 *   （`createHttpAuthProvider` の `DEFAULT_STORAGE_KEY`、
 *   `remember` 未指定時の既定保存先）に入る。
 */
import type { APIRequestContext, Locator, Page } from '@playwright/test';

/** 管理系 REST 全体で共通の CSRF ヘッダー（`$lib/banto/setup.ts::CSRF_HEADER` と同型）。 */
export const CSRF_HEADERS = { 'X-Banto-Client': 'banto' } as const;

/** `createHttpAuthProvider` の既定 `storageKey`（`remember` 未指定時の保存先）。 */
export const TOKEN_STORAGE_KEY = 'banto.auth.token';

/**
 * 両 spec ファイルが共有する唯一の管理者アカウント。`banto-hub-smoke.spec.ts`
 * が最初の1件目としてこの資格情報で初回セットアップ（`POST /api/auth/setup`）
 * を実 DOM 経由で行う想定（testMatch のファイル名順で smoke →
 * tags-continuous、workers:1/fullyParallel:false なので決定的に smoke が
 * 先に走る）。{@link ensureLoggedIn} は「未初期化なら setup、初期化済みなら
 * login」を自動判定するので、実行順が入れ替わっても2つ目の spec は同じ
 * 資格情報でログインできる（setup は一度しか成功しないため、想定と異なる
 * 順で実行された場合の保険）。
 * したがって `fetchAuthToken`/`ensureLoggedIn` を使う新規 spec は、ファイル名を
 * `banto-hub-smoke.spec.ts` より辞書順で後になるように付けること（smoke より
 * 先に走ると smoke の初回セットアップ DOM 検証を壊す）。
 */
export const HUB_ADMIN_USERNAME = 'e2e-hub-admin';
export const HUB_ADMIN_PASSWORD = 'E2eHubAdminPass1';
export const HUB_ADMIN_DISPLAY_NAME = 'E2Eハブ管理者';

interface AuthActionResponse {
	success: boolean;
	error?: string;
	token?: string;
}

/**
 * `GET /api/auth/status` → 未初期化なら `POST /api/auth/setup`、初期化済み
 * なら `POST /api/auth/login` のどちらかでベアラートークンを取得する。
 * `request`（`page.request`/`APIRequestContext`）は UI を経由せず直接 HTTP
 * を叩く - タグ連続登録 DOM テスト（`banto-hub-tags-continuous.spec.ts`）
 * では認証自体は本題ではないため、フォーム入力より速く安定させる。
 */
export async function fetchAuthToken(request: APIRequestContext): Promise<string> {
	const statusRes = await request.get('/api/auth/status', { headers: CSRF_HEADERS });
	const status = (await statusRes.json()) as { initialized: boolean };

	const response = status.initialized
		? await request.post('/api/auth/login', {
				headers: CSRF_HEADERS,
				data: { username: HUB_ADMIN_USERNAME, password: HUB_ADMIN_PASSWORD }
			})
		: await request.post('/api/auth/setup', {
				headers: CSRF_HEADERS,
				data: {
					username: HUB_ADMIN_USERNAME,
					password: HUB_ADMIN_PASSWORD,
					displayName: HUB_ADMIN_DISPLAY_NAME
				}
			});

	const body = (await response.json()) as AuthActionResponse;
	if (!body.success || !body.token) {
		const action = status.initialized ? 'login' : 'setup';
		throw new Error(`banto-hub ${action} に失敗しました: ${body.error ?? '不明なエラー'}`);
	}
	return body.token;
}

/**
 * 発行済みトークンを `createHttpAuthProvider` が読む `sessionStorage` へ
 * 書き込む。呼び出し前に `page.goto(...)` で banto-hub のオリジンへ一度
 * 遷移していること（`sessionStorage` はオリジンに紐づく - 遷移前は
 * `about:blank` で書き込み先が無い）。
 */
export async function injectAuthToken(page: Page, token: string): Promise<void> {
	await page.evaluate(
		([key, value]) => window.sessionStorage.setItem(key, value),
		[TOKEN_STORAGE_KEY, token]
	);
}

/**
 * `fetchAuthToken` + `injectAuthToken` をまとめたショートカット。
 * `(app)/+layout.ts` の認証ガード（`AuthProvider.check()`）はこの
 * `sessionStorage` を読むので、この関数の後に保護下のルートへ
 * `page.goto`/リンククリックすれば `/login` へ弾かれない。
 */
export async function ensureLoggedIn(page: Page): Promise<void> {
	const token = await fetchAuthToken(page.request);
	await injectAuthToken(page, token);
}

function escapeRegExp(value: string): string {
	return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * T19 S1-a（`banto-hub-tags-tree-context-menu.spec.ts` で発見・固定された
 * 罠、T19 S1-c で「新規登録」「連続登録」がグループ選択に紐づいたことで
 * ツリーのグループ選択が他 spec にも広がったため、ここへ切り出した共有
 * ヘルパー）: ツリーのグループ行のアクセシブル名は「名前 + タグ件数 + 周期」
 * の合成（例: `e2e-tcm-virtgrp-1730000000000 (1) 1000ms`）になるため、接続
 * ノードと違って `exact: true` の完全一致は決して成立しない。かといって
 * 単純な部分一致に戻すと、接尾辞だけが違う2つのグループ名（例:
 * `VIRT_GROUP` と `VIRT_GROUP_RENAMED`）が相互に部分一致してしまう。名前の
 * 直後が ` (`（件数の開始）であることまで確認して境界を明示することで、
 * 接尾辞ありでも一意に当てる。
 *
 * 接続ノードのラベルは名前そのものなので、そちらは `exact: true` で足りる
 * （このヘルパーは不要）。
 */
export function groupNodeByName(page: Page, name: string): Locator {
	return page
		.getByRole('tree')
		.getByRole('button', { name: new RegExp(`^${escapeRegExp(name)} \\(`) });
}
