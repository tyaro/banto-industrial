/**
 * loopback の origin に残った Remember me のトークンの掃除（#644 PR5、2026-11-14 オーナー決定）。
 *
 * 以前のビルド（や、決定前に保持したログイン）が `localStorage['banto.auth.token']` に置いた
 * 長寿命トークンは、Hub 停止中に同じ origin のポートを先取りした別プロセスのページの JS から
 * 読める。そこで loopback の origin では起動時（`createHttpAuthProvider()` の**前**）に、
 * そのトークンで `POST /api/auth/logout` を送ってサーバー側で失効させ、結果にかかわらず
 * `localStorage` から消す。`sessionStorage` には触らない。失敗（ネットワーク・401 等）は
 * 握りつぶして消すだけ。LAN など loopback でない origin では何もしない。
 */
import { isLoopbackOrigin } from './loopbackOrigin';

/** banto の `createHttpAuthProvider` の既定の保存キー。 */
const TOKEN_KEY = 'banto.auth.token';

/** banto の HTTP provider が付ける CSRF 用ヘッダ（`setup.ts` の `CSRF_HEADER` と同じ）。 */
const CLIENT_HEADER = { 'X-Banto-Client': 'banto' } as const;

/** Hub が応答しなくても bantoReady を止めないための上限（#644 PR5）。 */
const LOGOUT_TIMEOUT_MS = 3000;

export interface PurgeDeps {
	isLoopback?: () => boolean;
	/** 既定は `globalThis.localStorage`（無い・触れない環境では undefined）。 */
	storage?: Pick<Storage, 'getItem' | 'removeItem'> | undefined;
	fetchFn?: typeof fetch;
}

function defaultStorage(): PurgeDeps['storage'] {
	try {
		return typeof localStorage === 'undefined' ? undefined : localStorage;
	} catch {
		return undefined;
	}
}

export async function purgeLoopbackRememberedToken(deps: PurgeDeps = {}): Promise<void> {
	const isLoopback = deps.isLoopback ?? isLoopbackOrigin;
	if (!isLoopback()) return;
	const storage = 'storage' in deps ? deps.storage : defaultStorage();
	if (!storage) return;
	let token: string | null;
	try {
		token = storage.getItem(TOKEN_KEY);
	} catch {
		return;
	}
	if (!token) return;
	try {
		const fetchFn = deps.fetchFn ?? fetch;
		await fetchFn('/api/auth/logout', {
			method: 'POST',
			headers: { ...CLIENT_HEADER, Authorization: `Bearer ${token}` },
			signal: AbortSignal.timeout(LOGOUT_TIMEOUT_MS)
		});
	} catch {
		// 失効の要求に失敗しても、ここのトークンは消す。
	}
	try {
		storage.removeItem(TOKEN_KEY);
	} catch {
		// 消せない環境では何もできない。
	}
}
