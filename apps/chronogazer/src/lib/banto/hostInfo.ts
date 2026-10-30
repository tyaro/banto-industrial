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
 * - `location.hostname` が loopback（`127.0.0.0/8`・IPv4 に対応づけた loopback・`localhost`・`[::1]`）。LAN の IP で
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

/** 名前とループバックの IPv6 で、この PC を指す `location.hostname`（ブラウザは IPv6 を角括弧つきで返す）。 */
const LOOPBACK_HOSTNAMES: readonly string[] = ['localhost', '[::1]', '::1'];

/** 先頭ゼロなしの 10 進の 4 つ組（ブラウザの正規化が返す形）。 */
const IPV4_DOTTED = /^(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})$/;

/**
 * IPv4 に対応づけた IPv6 の、ブラウザが正規化して返す 16 進の形 `[::ffff:7fXX:XXXX]`
 * （`127.0.0.0/8` の範囲）。ドット付きの `::ffff:127.0.0.2` はブラウザの正規化では
 * 出てこないので受けない。
 */
const MAPPED_LOOPBACK = /^\[::ffff:7f[0-9a-f]{2}:[0-9a-f]{1,4}\]$/;

/**
 * `hostname` がこの PC（loopback）を指すか。サーバーの `request_is_loopback_local`
 * （`127.0.0.0/8` 全体と、IPv4 に対応づけた loopback）と同じ範囲に揃える（#942）。
 * ブラウザが正規化した形だけを受け、`0127.0.0.1`・`127.0.0.256`・`127.0.0.1.example`
 * のような形は断る。
 */
export function isLoopbackHostname(hostname: string): boolean {
	const h = hostname.toLowerCase();
	if (LOOPBACK_HOSTNAMES.includes(h) || MAPPED_LOOPBACK.test(h)) return true;
	const m = IPV4_DOTTED.exec(h);
	return m !== null && Number(m[1]) === 127 && m.slice(2).every((o) => Number(o) <= 255);
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
