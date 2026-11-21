/**
 * タグ登録の表の「現在値」「品質」「時刻」の列の表示（#1255 段階 ②）。
 *
 * タグ登録（`routes/(app)/tags/+page.svelte`）の行は管理 API の `Tag` で、現在値を持たない。
 * 値は WS の購読（`tagValueStream.svelte.ts`）が外部名（`{接続}.{グループ}.{タグ}`）をキーに
 * 持つ最新値のマップから引く。行の表示は、タグモニタと同じ {@link monitorCellDisplay}
 * （未確認・再接続中・止まっている間の扱いを含む）に通す。
 */
import type { RowValue } from './monitorValues';
import {
	monitorCellDisplay,
	type CellDisplayMode,
	type MonitorCellDisplay
} from './monitorStreamView';

/** 値をまだ受けていないタグの既定（モニタの `toRow` と同じ: 値なし・陳腐化・時刻 0）。 */
const NO_VALUE: RowValue = { v: null, q: 'stale', t: 0 };

/** 1 行の値・品質の表示。 */
export function tagValueCell(
	values: ReadonlyMap<string, RowValue>,
	externalName: string,
	unit: string | null,
	mode: CellDisplayMode
): MonitorCellDisplay {
	const entry = values.get(externalName) ?? NO_VALUE;
	return monitorCellDisplay({ ...entry, unit }, mode);
}

/** 時刻の列の表示（受信前は `--`。モニタの `formatTime` と同じ）。 */
export function tagValueTime(values: ReadonlyMap<string, RowValue>, externalName: string): string {
	const t = values.get(externalName)?.t ?? 0;
	if (!t) return '--';
	return new Date(t).toLocaleString('ja-JP');
}

/** 値の列の並べ替えに使う数値（値が出ない行は `null`）。 */
export function tagValueSortKey(
	values: ReadonlyMap<string, RowValue>,
	externalName: string
): number | null {
	const entry = values.get(externalName);
	if (!entry || entry.q !== 'good') return null;
	return entry.v;
}
