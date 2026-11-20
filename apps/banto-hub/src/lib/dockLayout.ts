/**
 * ドックの枠（`@banto/dock-svelte` の `ThreePaneDock`）の配置の保存値の、キーと読み込み（#1253。
 * docs/hub/banto-hub-ui-improvement-proposal.md §ドック型の配置）。runes を使わない純粋な
 * モジュールなので単体テストできる（`dockLayout.svelte.ts` が localStorage と
 * `UiSettingsProvider` への配線を持つ。`navLayout.ts` と `settings.svelte.ts` の分け方と同じ）。
 *
 * - 配置は利用者ごとの保存に任せ、URL には載せない（選択中の対象は今の `?target=` のまま）。
 * - キーはページごと（`/monitor` は `monitor`、`/tags` は `tags`）。
 * - ここでの検証は「自分が書いた形か」だけ（`ThreePaneSavedLayout` の外形: `schema: 1`・
 *   `panels`・`layout`）。中身の検証（知らない id・壊れた木・今のパネルとの照合）は部品の
 *   `reconcileThreePaneLayout` が行うので、ここでは繰り返さない。
 */

/** ページの名前（キーの一部）。`UiSettingsProvider` のキーは `[A-Za-z0-9._-]{1,64}`。 */
const PAGE_PATTERN = /^[a-z0-9-]{1,40}$/;

export interface DockLayoutKeys {
	/** localStorage の写し（ログイン前・サーバーの応答の前の最初の描画に使う）。 */
	local: string;
	/** `UiSettingsProvider`（Hub のサーバー、利用者ごと）のキー。本体。 */
	setting: string;
}

/** ページごとの保存先のキー。ページ名が不正なら例外（開発時の誤り）。 */
export function dockLayoutKeys(page: string): DockLayoutKeys {
	if (!PAGE_PATTERN.test(page)) throw new Error(`invalid dock layout page: ${page}`);
	return { local: `banto.dock.${page}`, setting: `hub.dock.${page}` };
}

/** 保存する値の外形（`ThreePaneSavedLayout` と同じ。中身は部品が照らす）。 */
export interface StoredDockLayout {
	schema: 1;
	panels: string[];
	layout: object;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** 保存値（JSON の文字列）-> 配置。未保存・壊れた値・知らない形は `undefined`（部品の既定に任せる）。 */
export function parseStoredDockLayout(
	raw: string | null | undefined
): StoredDockLayout | undefined {
	if (typeof raw !== 'string' || raw === '') return undefined;
	let value: unknown;
	try {
		value = JSON.parse(raw);
	} catch {
		return undefined;
	}
	if (!isRecord(value) || value.schema !== 1) return undefined;
	const { panels, layout } = value;
	if (!Array.isArray(panels) || !panels.every((id) => typeof id === 'string')) return undefined;
	if (!isRecord(layout)) return undefined;
	return { schema: 1, panels: [...panels], layout };
}

/** 配置 -> 保存する文字列（同じ配置なら同じ文字列。書き込みを省く比較にも使う）。 */
export function serializeDockLayout(layout: StoredDockLayout): string {
	return JSON.stringify(layout);
}
