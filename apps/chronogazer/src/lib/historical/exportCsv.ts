/**
 * ヒストリカル画面の「CSV 出力」の純ロジック（#1207 R3 PR3）。
 *
 * - 出力は表示中の表示グループ・期間（2026-11-19 オーナー決定）。生の値か、間隔で揃えた
 *   統計（平均・最大・最小）。横持ち（1 行 = 1 時刻、列 = タグ）。
 * - 先頭列は「時刻」（端末のローカル時刻）。タグの列の見出しは `名前` か `名前 [単位]`。
 * - Excel で開く前提: 数式として読まれる見出しは先頭に `'` を付け（`formulaSafe`）、BOM を付ける。
 */
import { csvFilename, csvForExcel, toCsv, type GridColumn } from '@banto/grid-svelte';
import {
	HISTORY_MAX_WINDOW_MS,
	type CollectExport,
	type CollectExportParams,
	type ExportStat
} from '../banto/collectAdmin';
import type { HistoryRange } from './historyLogic';

export const EXPORT_INTERVALS: readonly { ms: number; label: string }[] = [
	{ ms: 60_000, label: '1 分' },
	{ ms: 600_000, label: '10 分' },
	{ ms: 3_600_000, label: '1 時間' }
];

export const EXPORT_STATS: readonly { stat: ExportStat; label: string }[] = [
	{ stat: 'avg', label: '平均' },
	{ stat: 'max', label: '最大' },
	{ stat: 'min', label: '最小' }
];

export const EXPORT_RAW_NOTE = '記録したサンプルをそのまま出します（10 万行まで）';
export const EXPORT_UNAVAILABLE_MESSAGE = 'データを読み出せませんでした';

/** ダイアログの選択。 */
export type ExportChoice =
	{ mode: 'raw' } | { mode: 'interval'; intervalMs: number; stat: ExportStat };

export const DEFAULT_EXPORT_CHOICE: ExportChoice = {
	mode: 'interval',
	intervalMs: EXPORT_INTERVALS[0].ms,
	stat: 'avg'
};

/**
 * 読み出しの引数。間隔で揃えるときは開始を間隔の倍数（エポック基準）に切り下げる
 * （終了はそのまま）。切り下げで期間がサーバーの上限（HISTORY_MAX_WINDOW_MS）を
 * 超えるときは切り上げに替える（上限ちょうどの期間が 422 で断られないように）。
 * 切り上げが終了を超えるなら、元の開始のままにする。1 分・10 分・1 時間の境目は、JST のような「時間単位のずれ」の
 * 時刻帯ならローカルの分・時の境目と一致する。
 */
export function exportRequestFor(
	range: HistoryRange,
	tagIds: readonly number[],
	choice: ExportChoice
): CollectExportParams {
	if (choice.mode === 'raw') {
		return { tagIds, fromMs: range.fromMs, toMs: range.toMs, mode: 'raw' };
	}
	let fromMs = Math.floor(range.fromMs / choice.intervalMs) * choice.intervalMs;
	if (range.toMs - fromMs > HISTORY_MAX_WINDOW_MS) {
		const up = Math.ceil(range.fromMs / choice.intervalMs) * choice.intervalMs;
		fromMs = up <= range.toMs ? up : range.fromMs;
	}
	return {
		tagIds,
		fromMs,
		toMs: range.toMs,
		mode: 'interval',
		intervalMs: choice.intervalMs,
		stat: choice.stat
	};
}

const pad = (n: number, width = 2) => String(n).padStart(width, '0');

/** ローカル時刻の `YYYY/MM/DD HH:mm:ss`（`withMillis` なら `.SSS` 付き）。 */
export function exportTimeLabel(epochMs: number, withMillis: boolean): string {
	const d = new Date(epochMs);
	if (Number.isNaN(d.getTime())) return String(epochMs);
	const base = `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
	return withMillis ? `${base}.${pad(d.getMilliseconds(), 3)}` : base;
}

export interface ExportTagInfo {
	name: string;
	unit?: string | null;
}

/** 見出しが数式として読まれないようにする（`toCsv` は見出しを変えない）。 */
const FORMULA_START = /^[=+\-@\t\r\n＝＋－＠]/;
function safeHeader(text: string): string {
	return FORMULA_START.test(text) ? `'${text}` : text;
}

interface ExportRow {
	tMs: number;
	values: (number | null)[];
}

/** 列の見出し（重複は ` (#タグID)` を足して区別する。見出しが列の ID になるため）。 */
export function exportHeaders(
	tags: readonly { tagId: number }[],
	tagInfo: (tagId: number) => ExportTagInfo
): string[] {
	const used = new Set<string>(['時刻']);
	return tags.map(({ tagId }) => {
		const info = tagInfo(tagId);
		const base = safeHeader(info.unit ? `${info.name} [${info.unit}]` : info.name);
		let header = base;
		if (used.has(header)) header = `${base} (#${tagId})`;
		let n = 2;
		while (used.has(header)) header = `${base} (#${tagId}-${n++})`;
		used.add(header);
		return header;
	});
}

/** 読み出し結果 → Excel 向けの CSV 文字列（BOM 付き）。 */
export function buildExportCsv(
	result: CollectExport,
	tagInfo: (tagId: number) => ExportTagInfo
): string {
	const withMillis = result.mode === 'raw';
	const headers = exportHeaders(result.tags, tagInfo);
	const columns: GridColumn<ExportRow>[] = [
		{
			id: '時刻',
			header: '時刻',
			accessor: (row) => exportTimeLabel(row.tMs, withMillis)
		},
		...headers.map((header, index): GridColumn<ExportRow> => ({
			id: header,
			header,
			accessor: (row) => {
				const v = row.values[index];
				return typeof v === 'number' && Number.isFinite(v) ? v : null;
			}
		}))
	];
	return csvForExcel(toCsv(columns, result.rows, { headers: true, formulaSafe: true }));
}

/** ファイル名に使えない文字・制御文字を `_` にする。 */
function sanitize(text: string): string {
	// eslint-disable-next-line no-control-regex
	const cleaned = text.replace(/[\\/:*?"<>|\u0000-\u001f\u007f]/g, '_').trim();
	return cleaned === '' ? 'export' : cleaned;
}

/** ファイル名の基（`csvFilename` が日時と拡張子を足す）。 */
export function exportFileBase(groupName: string, choice: ExportChoice): string {
	const group = sanitize(groupName);
	if (choice.mode === 'raw') return `${group}-raw`;
	const interval = EXPORT_INTERVALS.find((i) => i.ms === choice.intervalMs);
	const intervalLabel = sanitize((interval?.label ?? `${choice.intervalMs}ms`).replace(/ /g, ''));
	const stat = EXPORT_STATS.find((s) => s.stat === choice.stat)?.label ?? choice.stat;
	return `${group}-${intervalLabel}-${stat}`;
}

export function exportFileName(groupName: string, choice: ExportChoice, now?: Date): string {
	return csvFilename(exportFileBase(groupName, choice), now);
}
