/**
 * ドックの枠（`@banto/dock-svelte` の `ThreePaneDock`）の配置の保存値の、キーと読み込み（#1253。
 * docs/hub/banto-hub-ui-improvement-proposal.md §ドック型の配置）。runes を使わない純粋な
 * モジュールなので単体テストできる（`dockLayoutStore.ts` が localStorage と
 * `UiSettingsProvider` への配線を持つ。`navLayout.ts` と `settings.svelte.ts` の分け方と同じ）。
 *
 * - 配置は利用者ごとの保存に任せ、URL には載せない（選択中の対象は今の `?target=` のまま）。
 * - キーはページごと（`/tags` は `tags`。もとあった `/monitor` の `monitor` は #1255 段階 ③で消え、保存の互換は持たない）。localStorage の写しは、さらに
 *   ログインの利用者ごと（`banto.dock.v2.<page>.<持ち主>`）。同じブラウザで別の利用者が
 *   ログインしたとき、前の利用者の配置を最初の描画に使わない（PR #1268 レビュー）。
 *   持ち主は `@banto/admin-core` の `SessionSnapshot.owner`（`account:<id>`・grant の kind）。
 *   持ち主が分からなければ写しのキーは無い（読まない・書かない）。前のページ名だけのキー
 *   `banto.dock.<page>` も読まない（移行しない）。
 * - キーには版（`DOCK_LAYOUT_KEY_VERSION`）を入れる（`hub.dock.v2.<page>`・
 *   `banto.dock.v2.<page>.<持ち主>`）。部品は保存値に無い追加パネルを「閉じた浮動」で足すので、
 *   下のログのパネル（#1253、追加パネル `log`）を足す前に保存した配置ではログが最初は
 *   出ない。v2 で保存値を一度捨て、既定の配置（ログを下に開く）から始める（#1268・#1269 は
 *   未リリースで、破壊的変更は許されている。前の版のキーは読まない・移行しない）。
 *   これからも、既定で開いておきたいパネルを足すときは版を上げる。
 * - ここでの検証は「自分が書いた形か」だけ（`ThreePaneSavedLayout` の外形: `schema: 1`・
 *   `panels`・`layout`）。中身の検証（知らない id・壊れた木・今のパネルとの照合）は部品の
 *   `reconcileThreePaneLayout` が行うので、ここでは繰り返さない。
 */

/** 保存のキーの版（上の doc。v2 = 下のログのパネルを足した、#1253）。 */
export const DOCK_LAYOUT_KEY_VERSION = 2;

/** ページの名前（キーの一部）。`UiSettingsProvider` のキーは `[A-Za-z0-9._-]{1,64}`。 */
const PAGE_PATTERN = /^[a-z0-9-]{1,40}$/;

export interface DockLayoutKeys {
	/**
	 * localStorage の写し（サーバーの応答の前の最初の描画に使う）。持ち主が分からなければ
	 * `undefined`（写しを使わない）。
	 */
	local: string | undefined;
	/** `UiSettingsProvider`（Hub のサーバー、利用者ごと）のキー。本体。 */
	setting: string;
}

/**
 * ページごと（写しはさらに持ち主ごと）の保存先のキー。ページ名が不正なら例外（開発時の誤り）。
 * 持ち主はキーの区切り（`.`）や記号を含みうるので `encodeURIComponent` で包む（`.` も
 * `%2E` にする。`account:1` は `account%3A1`）。
 */
export function dockLayoutKeys(page: string, owner: string | null): DockLayoutKeys {
	if (!PAGE_PATTERN.test(page)) throw new Error(`invalid dock layout page: ${page}`);
	const local =
		owner === null || owner === ''
			? undefined
			: `banto.dock.v${DOCK_LAYOUT_KEY_VERSION}.${page}.${encodeURIComponent(owner).replace(/\./g, '%2E')}`;
	return { local, setting: `hub.dock.v${DOCK_LAYOUT_KEY_VERSION}.${page}` };
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
