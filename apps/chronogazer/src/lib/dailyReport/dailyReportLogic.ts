/**
 * 日報画面の純粋なロジック（#1207 R3 日報 PR2）: 日付の扱い・URL の読み書き・
 * 帳票テンプレートの組み立て・読み出しの応答を帳票のデータにする処理。画面
 * （`routes/(app)/daily-report`）は、これを呼んで `@banto/report` の `ReportView` に渡すだけにする。
 *
 * - **1 日 = 端末のローカル 0:00 から翌日のローカル 0:00**（2026-11-19 オーナー決定、固定）。
 *   `new Date(y, m, d)` と `new Date(y, m, d + 1)` で作るので、夏時間の日は自然に 23 / 25 時間になる
 *   （サーバーの窓は 1〜25 時間）。
 * - **帳票は組み込みの固定テンプレート**（DB に持たない・編集できない）。`@banto/report` の
 *   テンプレートは行の繰り返しはできても列は繰り返せないので、タグの本数（1〜8）に合わせた
 *   テンプレートの文字列をここで組み立てる。
 * - **タグ名・単位・値はすべてデータ側に置く**（見出しも `{{ h0 }}` で差し込む）。テンプレートの
 *   文字列に混ぜないので、タグ名に `|` や `*`・`{{` があっても表が壊れない（差し込まれた値は
 *   エンジンが再解釈せず、HTML エスケープして出す）。
 * - 値は文字列に整形して渡す（小数桁はタグの設定、無ければ既定）。欠測は `—`。
 */
import { isProviderError } from '@banto/admin-core';
import {
	DAILY_REPORT_MAX_TAGS as SERVER_DAILY_REPORT_MAX_TAGS,
	type CollectDailyReport,
	type CollectDailyReportParams,
	type DailyReportStat
} from '../banto/collectAdmin';
import { formatValue } from '../monitor/monitorLogic';

/**
 * 日報に載せるタグの上限（サーバーの上限と同じ 8 本）。表示グループは 16 ペンまで持てるので、
 * 9 本以上のグループは画面でペンを選ばせる（`penSelection`）。
 */
export const DAILY_REPORT_MAX_TAGS = SERVER_DAILY_REPORT_MAX_TAGS;

/** 小数桁を読めない（タグ情報が未取得の）ときの桁数。 */
export const DAILY_REPORT_DEFAULT_DECIMALS = 2;

/** 欠測のセルの表示。 */
export const DAILY_REPORT_EMPTY_CELL = '—';

/**
 * 日報の読み出しを待つ上限（ミリ秒）。サーバーは長い読み出しの絞り
 * （30 秒、chronogazer core の `HISTORY_LONG_READ_TIMEOUT`）で自分の側を打ち切る。
 * それを十分に過ぎても何も返らないなら、アプリが応答していないか応答が届いていない
 * （`collectAdmin.ts` の `COLLECT_UI_TIMEOUT_MS` と同じ考え方）。
 * 読み込み中の表示のまま止まらないよう、画面側でも期限を切って再試行を出す。
 */
export const DAILY_REPORT_CLIENT_TIMEOUT_MS = 60000;

/** 期限切れの文言（秒数は期限から求める）。 */
export function dailyReportTimeoutMessage(
	timeoutMs: number = DAILY_REPORT_CLIENT_TIMEOUT_MS
): string {
	return `日報の読み出しが ${Math.round(timeoutMs / 1000)} 秒以内に終わりませんでした。もう一度試してください。`;
}

// --- 日付 --------------------------------------------------------------------

/** `YYYY-MM-DD`（`<input type="date">` と URL の形）。実在する日付だけ受ける。 */
export function parseDateParam(raw: string | null): { y: number; m: number; d: number } | null {
	if (raw === null) return null;
	const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(raw);
	if (!match) return null;
	const y = Number(match[1]);
	const m = Number(match[2]);
	const d = Number(match[3]);
	const date = new Date(y, m - 1, d);
	// 2 月 30 日などは Date が繰り上げるので、往復して同じかを見る。
	if (date.getFullYear() !== y || date.getMonth() !== m - 1 || date.getDate() !== d) return null;
	return { y, m, d };
}

function pad2(n: number): string {
	return String(n).padStart(2, '0');
}

/** 端末のローカル日付 → `YYYY-MM-DD`。 */
export function toDateValue(date: Date): string {
	return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())}`;
}

/** URL の日付 → 表示する日付。読めなければ今日（`fellBack`。URL に何も無いときは `false`）。 */
export function resolveDate(
	raw: string | null,
	nowMs: number
): { date: string; fellBack: boolean } {
	if (raw !== null && parseDateParam(raw) !== null) return { date: raw, fellBack: false };
	return { date: toDateValue(new Date(nowMs)), fellBack: raw !== null };
}

/** その日のローカル 0:00 〜 翌日のローカル 0:00（ミリ秒）。日付が不正なら `null`。 */
export function dayRange(date: string): { fromMs: number; toMs: number } | null {
	const parsed = parseDateParam(date);
	if (parsed === null) return null;
	return {
		fromMs: new Date(parsed.y, parsed.m - 1, parsed.d).getTime(),
		toMs: new Date(parsed.y, parsed.m - 1, parsed.d + 1).getTime()
	};
}

/** `days` 日ずらした日付（カレンダー上で数える。夏時間でずれない）。不正なら `null`。 */
export function shiftDate(date: string, days: number): string | null {
	const parsed = parseDateParam(date);
	if (parsed === null) return null;
	return toDateValue(new Date(parsed.y, parsed.m - 1, parsed.d + days));
}

/** 「次の日」へ進めるか（明日以降は記録が無いので今日まで）。 */
export function canShiftNextDay(date: string, nowMs: number): boolean {
	const next = shiftDate(date, 1);
	return next !== null && next <= toDateValue(new Date(nowMs));
}

const WEEKDAYS = ['日', '月', '火', '水', '木', '金', '土'];

/** 帳票に出す日付（`2026-11-19（木）`）。不正ならそのまま返す。 */
export function dateLabel(date: string): string {
	const parsed = parseDateParam(date);
	if (parsed === null) return date;
	const weekday = WEEKDAYS[new Date(parsed.y, parsed.m - 1, parsed.d).getDay()];
	return `${date}（${weekday}）`;
}

/** 画面の URL のクエリ文字列（`?group=…&date=…`）。 */
export function dailyReportQuery(groupId: number, date: string): string {
	return `?group=${groupId}&date=${date}`;
}

// --- 読み出しの引数 ------------------------------------------------------------

/** 日・タグから読み出しの引数を作る。タグが無い・日付が不正なら `null`（読みに行かない）。 */
export function dailyReportRequestFor(
	date: string,
	tagIds: readonly number[]
): CollectDailyReportParams | null {
	const range = dayRange(date);
	if (range === null || tagIds.length === 0) return null;
	return { tagIds: tagIds.slice(0, DAILY_REPORT_MAX_TAGS), ...range };
}

// --- 帳票 --------------------------------------------------------------------

/** 帳票の列に使うタグの情報（画面がタグ ID から引く）。 */
export interface ReportTagInfo {
	name: string;
	unit: string | null;
	/** 小数桁（タグを読めていなければ `null`）。 */
	decimals: number | null;
}

/** 列の見出し（`名前 [単位]`、単位が無ければ名前だけ）。 */
export function columnHeader(info: ReportTagInfo): string {
	return info.unit ? `${info.name} [${info.unit}]` : info.name;
}

/** 1 つの値の整形。欠測は `—`、小数桁はタグの設定（読めなければ既定）。 */
export function formatReportValue(value: number | null, decimals: number | null): string {
	if (value === null || !Number.isFinite(value)) return DAILY_REPORT_EMPTY_CELL;
	return formatValue(value, decimals ?? DAILY_REPORT_DEFAULT_DECIMALS);
}

/** 1 時間ごとの行の時刻（端末のローカル時刻 `HH:mm`）。 */
export function hourLabel(tMs: number): string {
	const date = new Date(tMs);
	return `${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
}

/**
 * 固定テンプレート（タグ `tagCount` 本、1〜8 に丸める）。タイトル・日付・グループ名・列の見出し・
 * 値はすべて `{{ }}` の差し込みで、テンプレートの文字列自体はタグの内容に依存しない。
 */
export function buildReportTemplate(tagCount: number): string {
	const n = Math.min(DAILY_REPORT_MAX_TAGS, Math.max(1, Math.trunc(tagCount)));
	const idx = Array.from({ length: n }, (_, i) => i);
	const heads = idx.map((i) => `{{ h${i} }}`).join(' | ');
	const cells = idx.map((i) => `{{ v${i} }}`).join(' | ');
	const aligns = idx.map(() => '---:').join('|');
	return [
		'# 日報',
		'',
		'日付: {{ date }}',
		'',
		'表示グループ: {{ group }}',
		'',
		'## 時間別（平均）',
		'',
		`| 時刻 | ${heads} |`,
		`|---|${aligns}|`,
		'{{#each hours}}',
		`| {{ time }} | ${cells} |`,
		'{{/each}}',
		'',
		'## 1 日の集計',
		'',
		`| 項目 | ${heads} |`,
		`|---|${aligns}|`,
		'{{#each summary}}',
		`| {{ label }} | ${cells} |`,
		'{{/each}}',
		''
	].join('\n');
}

/** 行 1 つ: `time`（時間別）か `label`（集計）と、値 `v0`…`v{n-1}`（整形済みの文字列）。 */
export type ReportRow = Record<string, string>;

/** 帳票に渡すデータ（`buildReportTemplate` のテンプレートと対）。列の見出しは `h0`…`h{n-1}`。 */
export type ReportData = {
	date: string;
	group: string;
	hours: ReportRow[];
	summary: ReportRow[];
} & Record<string, string | ReportRow[]>;

function statValues(
	stats: readonly (DailyReportStat | null)[],
	pick: (stat: DailyReportStat) => number,
	infos: readonly ReportTagInfo[]
): ReportRow {
	const out: ReportRow = {};
	infos.forEach((info, i) => {
		const stat = stats[i] ?? null;
		out[`v${i}`] = formatReportValue(stat === null ? null : pick(stat), info.decimals);
	});
	return out;
}

/**
 * 読み出しの応答 → 帳票のテンプレートとデータ（純関数）。列は応答の `tags` の並び（削除された
 * タグは応答に入らない）。時間別は平均、1 日の集計は平均・最大・最小。
 */
export function buildReportData(
	report: CollectDailyReport,
	groupName: string,
	date: string,
	tagInfo: (tagId: number) => ReportTagInfo
): { template: string; data: ReportData; tagCount: number } {
	const infos = report.tags.slice(0, DAILY_REPORT_MAX_TAGS).map((tag) => tagInfo(tag.tagId));
	const data: ReportData = {
		date: dateLabel(date),
		group: groupName,
		hours: report.hours.map((hour) => ({
			time: hourLabel(hour.tMs),
			...statValues(hour.values, (s) => s.avg, infos)
		})),
		summary: [
			{ label: '平均', ...statValues(report.day, (s) => s.avg, infos) },
			{ label: '最大', ...statValues(report.day, (s) => s.max, infos) },
			{ label: '最小', ...statValues(report.day, (s) => s.min, infos) }
		]
	};
	infos.forEach((info, i) => {
		data[`h${i}`] = columnHeader(info);
	});
	return { template: buildReportTemplate(infos.length), data, tagCount: infos.length };
}

/** その日に記録された値が 1 つも無いか（全部欠測）。 */
export function isEmptyReport(report: Pick<CollectDailyReport, 'hours' | 'day'>): boolean {
	return (
		report.day.every((stat) => stat === null) &&
		report.hours.every((hour) => hour.values.every((value) => value === null))
	);
}

// --- 注記・エラー ------------------------------------------------------------

export const DAILY_REPORT_UNAVAILABLE_MESSAGE =
	'データを読み出せませんでした。しばらくしてからもう一度試してください。';
export const DAILY_REPORT_EMPTY_MESSAGE = 'この日に記録された値はありません。';
export const DAILY_REPORT_LIMIT_MESSAGE =
	'日報の対象がサーバーの上限（タグ 8 本・窓 25 時間）を超えています。表示グループを見直してください。';

/** 読み出しが投げたもの → 日本語の文言（422 も日本語にする）。 */
export function dailyReportErrorMessage(err: unknown): string {
	if (isProviderError(err)) {
		if (err.body.kind === 'validation') return DAILY_REPORT_LIMIT_MESSAGE;
		return `日報の材料を読み出せませんでした（${err.message}）。`;
	}
	return `日報の材料を読み出せませんでした（${err instanceof Error ? err.message : String(err)}）。`;
}

/** 不明なタグ（削除された）の注記。無ければ `null`。 */
export function unknownTagsNote(
	unknownTagIds: readonly number[],
	nameOf: (tagId: number) => string
): string | null {
	if (unknownTagIds.length === 0) return null;
	return `削除されたタグがあります（${unknownTagIds.map(nameOf).join('、')}）。日報には載せていません。`;
}

/**
 * シミュレーション接続のタグの注記。無ければ `null`。
 * `simulation` は**現在の設定**であって履歴ではない。シミュレーション中の値は記録されないので
 * その時間帯は欠測（「—」）になるが、切り替える前に記録した値は日報に出る。
 */
export function simulationNote(
	report: Pick<CollectDailyReport, 'tags'>,
	nameOf: (tagId: number) => string
): string | null {
	const names = report.tags.filter((tag) => tag.simulation).map((tag) => nameOf(tag.tagId));
	if (names.length === 0) return null;
	return `現在シミュレーション接続のタグがあります（${names.join('、')}）。シミュレーション中の値は記録されないため、その間は「—」になります。切り替える前に記録した値は表示されます。`;
}
