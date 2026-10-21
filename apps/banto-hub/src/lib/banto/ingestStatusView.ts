/**
 * R2a-2e（docs/banto-hub-ingest-design.md §9「状態の表示」）: `push` 接続の
 * 受信の状態（`GET /api/status` の `ingest` 節、`hubStatus.ts::IngestStatusEntry`）を
 * 画面の文言にする純関数。`ConnectionDrawer.svelte`（ドロワーの状態）と
 * `status/+page.svelte`（接続の一覧の「送り手待ち」）が使う。
 *
 * - 接続の状態（`connections[]`）は収集の外部接続の規則のまま `connected` /
 *   `reconnecting`（`attempt: 0`）を出す（§9）。`push` 接続では「再接続中」では
 *   なく、`reconnecting` を「送り手待ち」、`connected` を「受信中」と読む
 *   （{@link pushConnectionStatusLabel}）。
 * - 送り手の `status` の文はサーバーが制御文字を落として 1 KiB に切り詰めて
 *   いるが、画面でも同じく切り詰める（{@link truncateSenderStatus}。古い
 *   サーバー・想定外の長さで画面が崩れないように）。
 *
 * このモジュールは型以外を import しない（`hubStatus.ts` は `@banto/admin-core`
 * を値で import するため）。
 */
import type { IngestSenderStatusEntry, IngestStatusEntry } from './hubStatus';

/** 送り手の `status` の表示の上限（バイト、`PUSH_LIMITS.max_status_detail_bytes`）。 */
export const SENDER_STATUS_MAX_BYTES = 1024;

const UTF8 = new TextEncoder();

/**
 * 送り手の `status` の文を表示用に整える: 制御文字を落とし、UTF-8 で
 * `maxBytes` バイトに文字の境界で切り詰める（切り詰めたら末尾に `…`）。
 * サーバーの `ingest::ws::sanitize_status` と同じ規則（`…` だけが画面の足し）。
 */
export function truncateSenderStatus(text: string, maxBytes = SENDER_STATUS_MAX_BYTES): string {
	// eslint-disable-next-line no-control-regex
	const clean = text.replace(/[\u0000-\u001f\u007f-\u009f]/g, '');
	if (UTF8.encode(clean).length <= maxBytes) return clean;
	let out = '';
	let bytes = 0;
	for (const ch of clean) {
		const size = UTF8.encode(ch).length;
		if (bytes + size > maxBytes) break;
		out += ch;
		bytes += size;
	}
	return `${out}…`;
}

/** 受信の状態の見出し（Live / Down、§4.3）。 */
export function ingestLinkLabel(entry: Pick<IngestStatusEntry, 'link' | 'enabled'>): string {
	if (!entry.enabled) return '無効（構成）';
	return entry.link === 'live' ? '受信中' : '送り手待ち';
}

/** 見出しの色の区分（`ok` / `warn` / `muted`）。 */
export function ingestLinkClass(
	entry: Pick<IngestStatusEntry, 'link' | 'enabled'>
): 'ok' | 'warn' | 'muted' {
	if (!entry.enabled) return 'muted';
	return entry.link === 'live' ? 'ok' : 'warn';
}

/** epoch ms の表示（`null` は「-」）。 */
export function formatIngestTime(value: number | null): string {
	return value === null ? '-' : new Date(value).toLocaleString('ja-JP');
}

/** 送り手 1 つの表示（「WS: db-bridge（127.0.0.1:53122）2026/10/21 9:00:00 から」）。 */
export function describeIngestSender(sender: IngestSenderStatusEntry): string {
	const peer = sender.peer ? `（${sender.peer}）` : '';
	if (sender.transport === 'ws') {
		return `WebSocket: ${sender.api_key_name}${peer} ${formatIngestTime(sender.connected_at)} から接続中`;
	}
	return `REST: ${sender.api_key_name}${peer} 最後の送信 ${formatIngestTime(sender.last_seen_at)}`;
}

/**
 * 接続の一覧（`connections[].status`）の `push` 接続での読み方（§9「UI は
 * `push` 接続では `attempt: 0` を「送り手待ち」と表示」）。`push` でない接続・
 * 対応の無い状態は `null`（呼び出し側の既定の文言を使う）。
 */
export function pushConnectionStatusLabel(status: string): string | null {
	if (status === 'reconnecting') return '送り手待ち';
	if (status === 'connected') return '受信中';
	return null;
}
