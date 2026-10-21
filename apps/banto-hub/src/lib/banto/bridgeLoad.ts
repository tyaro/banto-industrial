/**
 * R2a-2e（docs/banto-hub-ingest-design.md §7.1 の 3.「UI に見積もりを出す」）:
 * `push` 接続のブリッジの区画が送る量の見積もり。`banto_tags::push::bridge_load`
 * （`crates/banto-tags/src/push.rs`）の式をそのまま写したもので、登録時に
 * サーバーが断る設定（上限の 8 割を超える、§12 Q20）を**保存の前に**画面で
 * 赤く示すために使う（サーバーの 422 を待たない。最終の判定はサーバー）。
 *
 * ```text
 * item_bytes(q) = Σ (key の UTF-8 バイト数 + 96)
 * msgs(q)       = max(ceil(件数(q) / 1000), ceil((item_bytes(q) + 512) / (256 KiB − 512)))
 * messages/分   = Σ msgs(q) × (60000 / period_ms(q))   ≤ 16 × 60（20 の 8 割）
 * items/分      = Σ 件数(q) × (60000 / period_ms(q))   ≤ 4,000 × 60（5,000 の 8 割）
 * ```
 *
 * 頻度は Rust と同じく**1 分あたりの整数**で持つ（周期の集合 100〜60000 ms は
 * どれも 60000 を割り切るので、境界の比較を浮動小数の丸めに頼らない）。
 *
 * **Rust と同じ値を出すことは fixtures で確かめる**: `bridgeLoad.fixtures.json`
 * を `bridgeLoad.test.ts`（このファイル）と
 * `apps/banto-hub/core/src/ingest/bridge_load_fixtures.rs`（`banto_tags::bridge_load`）
 * の両方が読み、同じ `expected` になることを確かめる。式・定数・文言を変える
 * ときは両方を同じ PR で直し、fixtures を Rust 側で作り直す
 * （`UPDATE_BRIDGE_LOAD_FIXTURES=1`）。
 *
 * このモジュールは何も import しない純関数のまま保つ（`plcConnectionForm.ts`
 * と同じ方針）。
 */

/**
 * 取り込みの上限のうち、見積もりと画面が使うもの - mirrors
 * `banto_tags::push::PUSH_LIMITS`（§7 の表）。
 */
export const PUSH_LIMITS = {
	maxItemsPerMessage: 1000,
	maxWsMessageBytes: 256 * 1024,
	messagesPerSec: 20,
	itemsPerSec: 5000,
	maxStatusDetailBytes: 1024,
	maxSettingsBytes: 64 * 1024,
	registrationHeadroomPercent: 80,
	bridgeItemOverheadBytes: 96,
	bridgeMessageOverheadBytes: 512
} as const;

/** 登録時に許すメッセージの頻度（持続の 8 割、16 / 秒）。 */
export const REGISTRATION_MESSAGES_PER_SEC = Math.floor(
	(PUSH_LIMITS.messagesPerSec * PUSH_LIMITS.registrationHeadroomPercent) / 100
);

/** 登録時に許す件数の頻度（持続の 8 割、4,000 / 秒）。 */
export const REGISTRATION_ITEMS_PER_SEC = Math.floor(
	(PUSH_LIMITS.itemsPerSec * PUSH_LIMITS.registrationHeadroomPercent) / 100
);

/** 見積もりの入力のクエリ 1 本: 周期と、送るキー（`columns[].key`）の並び。 */
export interface BridgeLoadQueryInput {
	periodMs: number;
	keys: readonly string[];
}

/** クエリ 1 本分の内訳（mirrors `banto_tags::push::QueryLoad`）。 */
export interface QueryLoad {
	/** 1 周期に送る件数。 */
	items: number;
	/** 1 周期の件の JSON の最悪の大きさの見積もり（バイト）。 */
	itemBytes: number;
	/** 1 周期のメッセージ数。 */
	messagesPerCycle: number;
}

/** 接続全体の見積もり（mirrors `banto_tags::push::BridgeLoad`）。 */
export interface BridgeLoad {
	queries: QueryLoad[];
	messagesPerMinute: number;
	itemsPerMinute: number;
	/** 1 秒あたりのメッセージ数の表示（`BridgeLoad::messages_per_sec_text`）。 */
	messagesPerSecText: string;
	/** 1 秒あたりの件数の表示（`BridgeLoad::items_per_sec_text`）。 */
	itemsPerSecText: string;
	/** 登録時の上限（持続の 8 割）に収まるか。ちょうどは通す。 */
	withinLimit: boolean;
	/** 上限を超えるときのサーバーと同じ文言（`over_registration_limit_message`）。収まるなら `null`。 */
	message: string | null;
}

const UTF8 = new TextEncoder();

/** Rust の `str::len()`（UTF-8 のバイト数）。 */
function utf8Bytes(text: string): number {
	return UTF8.encode(text).length;
}

/** 3 桁区切り（Rust の `group_thousands`）。 */
export function groupThousands(n: number): string {
	const digits = String(n);
	let out = '';
	for (let i = 0; i < digits.length; i++) {
		if (i > 0 && (digits.length - i) % 3 === 0) out += ',';
		out += digits[i];
	}
	return out;
}

/**
 * 1 分あたりの数を 1 秒あたりにして、小数 1 桁まで（切り上げ、整数なら小数
 * なし）で書く（Rust の `format_rate`）。
 */
export function formatRatePerSec(perMinute: number): string {
	const tenths = Math.ceil((perMinute * 10) / 60);
	const whole = Math.floor(tenths / 10);
	const frac = tenths % 10;
	return frac === 0 ? groupThousands(whole) : `${groupThousands(whole)}.${frac}`;
}

/** §7.1 の式（`banto_tags::push::bridge_load`）。 */
export function bridgeLoad(queries: readonly BridgeLoadQueryInput[]): BridgeLoad {
	const bytesPerMessage = PUSH_LIMITS.maxWsMessageBytes - PUSH_LIMITS.bridgeMessageOverheadBytes;
	let messagesPerMinute = 0;
	let itemsPerMinute = 0;
	const loads: QueryLoad[] = queries.map((query) => {
		const items = query.keys.length;
		let itemBytes = 0;
		for (const key of query.keys) itemBytes += utf8Bytes(key) + PUSH_LIMITS.bridgeItemOverheadBytes;
		const byItems = Math.ceil(items / PUSH_LIMITS.maxItemsPerMessage);
		const byBytes = Math.ceil(
			(itemBytes + PUSH_LIMITS.bridgeMessageOverheadBytes) / bytesPerMessage
		);
		const messagesPerCycle = Math.max(byItems, byBytes);
		const cyclesPerMinute = query.periodMs > 0 ? Math.floor(60000 / query.periodMs) : 1;
		messagesPerMinute += messagesPerCycle * cyclesPerMinute;
		itemsPerMinute += items * cyclesPerMinute;
		return { items, itemBytes, messagesPerCycle };
	});
	const withinLimit =
		messagesPerMinute <= REGISTRATION_MESSAGES_PER_SEC * 60 &&
		itemsPerMinute <= REGISTRATION_ITEMS_PER_SEC * 60;
	const messagesPerSecText = formatRatePerSec(messagesPerMinute);
	const itemsPerSecText = formatRatePerSec(itemsPerMinute);
	return {
		queries: loads,
		messagesPerMinute,
		itemsPerMinute,
		messagesPerSecText,
		itemsPerSecText,
		withinLimit,
		message: withinLimit
			? null
			: `この設定は 1 秒あたり約 ${messagesPerSecText} メッセージ・${itemsPerSecText} 件になり、上限（${groupThousands(REGISTRATION_MESSAGES_PER_SEC)} メッセージ・${groupThousands(REGISTRATION_ITEMS_PER_SEC)} 件）を超えます。周期を長くするか、クエリを別の push 接続に分けてください`
	};
}
