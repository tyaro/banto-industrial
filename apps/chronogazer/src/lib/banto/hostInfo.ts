/**
 * いまの画面がどの host に繋がっているか（`GET /api/host`、#392 B1-4）と、REST の
 * 設定（`/api/settings/*`）をこの画面から変えられるか（#392 B2-2、2026-10-14 オーナー
 * 決定 4）。
 *
 * サーバーは `PUT /api/settings/server`・`PUT /api/settings/auth` を「サービスの host
 * （`chronogazer-server`）への、この PC からの要求（peer と Host の両方が loopback）」
 * だけで受け付け、それ以外は `403`（B2-1）。それが本当の境界で、画面は同じ条件を
 * 先に見て、変えられないときは入力欄の代わりに読むだけの表示と理由を出す:
 *
 * - `hostKind` が `server`（コンソール）か `service`（Windows サービス）。デスクトップ
 *   （`desktop`）は Tauri の経路で設定し、`banto-serve`（`serve`）は開発用で REST の
 *   変更を受けない。
 * - `location.hostname` が loopback（`127.0.0.1`・`localhost`・`[::1]`）。LAN の IP で
 *   開いた画面は LAN の listener 経由なので変えられない。
 *
 * `GET /api/host` は認証なしで読め、起動の間は変わらないので 1 回だけ読んで覚える
 * （失敗は覚えず、次に呼ばれたとき読み直す）。
 */
import { restJson } from './restJson';

/** `chronogazer_core::host_info::HostInfo`。 */
export interface HostInfo {
	product: string;
	/** `desktop` / `serve` / `server` / `service`。 */
	hostKind: string;
	version: string;
	scope: string;
}

/** 設定の REST を受け付ける host 種別。 */
export const REST_SETTINGS_HOST_KINDS: readonly string[] = ['server', 'service'];

/** この PC を指す `location.hostname`（ブラウザは IPv6 を角括弧つきで返す）。 */
const LOOPBACK_HOSTNAMES: readonly string[] = ['127.0.0.1', 'localhost', '[::1]', '::1'];

/** `hostname` がこの PC（loopback）を指すか。 */
export function isLoopbackHostname(hostname: string): boolean {
	return LOOPBACK_HOSTNAMES.includes(hostname.toLowerCase());
}

/**
 * 設定の REST をこの画面から変えられるか（純関数。モジュール doc）。`hostKind` が
 * 読めていない（`undefined`）なら変えられない側に倒す。
 */
export function settingsEditableHere(hostKind: string | undefined, hostname: string): boolean {
	return (
		hostKind !== undefined &&
		REST_SETTINGS_HOST_KINDS.includes(hostKind) &&
		isLoopbackHostname(hostname)
	);
}

let cached: Promise<HostInfo> | null = null;

/** `GET /api/host` を読む（成功したら覚える）。 */
export function getHostInfo(): Promise<HostInfo> {
	if (cached) return cached;
	const request = restJson<HostInfo>('/api/host', { method: 'GET', anonymous: true });
	cached = request;
	request.catch(() => {
		if (cached === request) cached = null;
	});
	return request;
}

/** テスト用: 覚えた結果を捨てる。 */
export function resetHostInfoCache(): void {
	cached = null;
}
