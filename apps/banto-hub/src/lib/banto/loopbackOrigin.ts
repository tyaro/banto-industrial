/**
 * 同じ PC（loopback）からの接続かどうかの判定（#644 PR5、2026-11-14 オーナー決定）。
 *
 * Hub が止まっている間に 127.0.0.1 のポートを別のプロセスが先取りすると、利用者が同じ
 * origin を開いたとき、そのページの JS は `localStorage` を読める。そのため loopback の
 * origin では「ログイン状態を保持」（Remember me、`localStorage` の長寿命トークン）を
 * 使わない。判定の範囲は ChronoGazer の `hub_tls.rs` の `is_loopback` に合わせる:
 * 127.0.0.0/8、`::1`、`::ffff:127.x.x.x`（16 進表記の `::ffff:7f00:1` を含む）、
 * `localhost`（大小無視・末尾の `.` 無視）、`*.localhost`。http/https・ポートは問わない。
 */

/** `URL.hostname` / `location.hostname`（IPv6 は角括弧つきでも無しでも可）が loopback か。 */
export function isLoopbackHost(hostname: string): boolean {
	let host = hostname.trim().toLowerCase();
	if (host.startsWith('[') && host.endsWith(']')) host = host.slice(1, -1);
	if (host.includes(':')) return isLoopbackIpv6(host);
	const v4 = parseIpv4(host);
	if (v4) return v4[0] === 127;
	const name = host.replace(/\.+$/, '');
	return name === 'localhost' || name.endsWith('.localhost');
}

function parseIpv4(host: string): number[] | null {
	const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
	if (!m) return null;
	const parts = m.slice(1).map(Number);
	return parts.every((n) => n <= 255) ? parts : null;
}

function isLoopbackIpv6(host: string): boolean {
	if (host === '::1' || host === '0:0:0:0:0:0:0:1') return true;
	// `::ffff:127.0.0.1`（10 進）と `::ffff:7f00:1`（16 進。URL の hostname はこちらに正規化される）。
	const dotted = /^(?:0:0:0:0:0:|::)ffff:(\d{1,3}(?:\.\d{1,3}){3})$/.exec(host);
	if (dotted) return parseIpv4(dotted[1])?.[0] === 127;
	const hex = /^(?:0:0:0:0:0:|::)ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(host);
	if (hex) return parseInt(hex[1], 16) >> 8 === 127;
	return false;
}

/** いまのページの origin が loopback か。ブラウザ以外（SSR・テスト）では false。 */
export function isLoopbackOrigin(): boolean {
	if (typeof location === 'undefined') return false;
	return isLoopbackHost(location.hostname);
}
