import { describe, expect, it } from 'vitest';
import { ProviderError } from '@banto/admin-core';
import { renderReport } from '@banto/report';
import type { CollectDailyReport } from '../banto/collectAdmin';
import {
	DAILY_REPORT_CLIENT_TIMEOUT_MS,
	dailyReportTimeoutMessage,
	DAILY_REPORT_EMPTY_CELL,
	DAILY_REPORT_LIMIT_MESSAGE,
	buildReportData,
	buildReportTemplate,
	canShiftNextDay,
	columnHeader,
	dailyReportErrorMessage,
	dailyReportQuery,
	dailyReportRequestFor,
	dateLabel,
	dayRange,
	formatReportValue,
	hourLabel,
	isEmptyReport,
	parseDateParam,
	resolveDate,
	shiftDate,
	simulationNote,
	toDateValue,
	unknownTagsNote
} from './dailyReportLogic';

const stat = (avg: number, min = avg - 1, max = avg + 1) => ({ min, max, avg, count: 10 });

function report(over: Partial<CollectDailyReport> = {}): CollectDailyReport {
	const from = new Date(2026, 10, 19).getTime();
	return {
		fromMs: from,
		toMs: new Date(2026, 10, 20).getTime(),
		intervalMs: 3_600_000,
		tags: [
			{ tagId: 1, simulation: false },
			{ tagId: 2, simulation: false }
		],
		unknownTagIds: [],
		hours: Array.from({ length: 24 }, (_, h) => ({
			tMs: new Date(2026, 10, 19, h).getTime(),
			values: [h === 3 ? null : stat(h), stat(100 + h)]
		})),
		day: [stat(12.345, 0, 23), null],
		...over
	};
}

const info = (tagId: number) => ({
	name: tagId === 1 ? '温度' : '圧力',
	unit: tagId === 1 ? '℃' : null,
	decimals: tagId === 1 ? 1 : null
});

describe('dailyReportTimeoutMessage', () => {
	it('既定は期限の秒数を載せる', () => {
		expect(DAILY_REPORT_CLIENT_TIMEOUT_MS).toBe(60000);
		expect(dailyReportTimeoutMessage()).toBe(
			'日報の読み出しが 60 秒以内に終わりませんでした。もう一度試してください。'
		);
	});
	it('期限を渡すとその秒数になる', () => {
		expect(dailyReportTimeoutMessage(45000)).toContain('45 秒');
	});
});

describe('日付', () => {
	it('parseDateParam は実在する YYYY-MM-DD だけ受ける', () => {
		expect(parseDateParam('2026-11-19')).toEqual({ y: 2026, m: 11, d: 19 });
		expect(parseDateParam('2026-02-30')).toBeNull();
		expect(parseDateParam('2026-1-9')).toBeNull();
		expect(parseDateParam('abc')).toBeNull();
		expect(parseDateParam(null)).toBeNull();
	});

	it('resolveDate: 正しければそのまま、読めなければ今日（URL に無いときは fellBack でない）', () => {
		const now = new Date(2026, 10, 19, 15, 30).getTime();
		expect(resolveDate('2026-11-01', now)).toEqual({ date: '2026-11-01', fellBack: false });
		expect(resolveDate(null, now)).toEqual({ date: '2026-11-19', fellBack: false });
		expect(resolveDate('2026-13-01', now)).toEqual({ date: '2026-11-19', fellBack: true });
	});

	it('dayRange はローカル 0:00 から翌日のローカル 0:00', () => {
		const range = dayRange('2026-11-19');
		expect(range).toEqual({
			fromMs: new Date(2026, 10, 19).getTime(),
			toMs: new Date(2026, 10, 20).getTime()
		});
		expect(dayRange('x')).toBeNull();
		// 月・年をまたぐ。
		expect(dayRange('2026-12-31')?.toMs).toBe(new Date(2027, 0, 1).getTime());
	});

	it('shiftDate はカレンダー上で日を数える', () => {
		expect(shiftDate('2026-11-19', -1)).toBe('2026-11-18');
		expect(shiftDate('2026-11-30', 1)).toBe('2026-12-01');
		expect(shiftDate('2026-03-01', -1)).toBe('2026-02-28');
		expect(shiftDate('bad', 1)).toBeNull();
	});

	it('canShiftNextDay は今日まで', () => {
		const now = new Date(2026, 10, 19, 12).getTime();
		expect(canShiftNextDay('2026-11-18', now)).toBe(true);
		expect(canShiftNextDay('2026-11-19', now)).toBe(false);
	});

	it('toDateValue・dateLabel・dailyReportQuery', () => {
		expect(toDateValue(new Date(2026, 0, 5))).toBe('2026-01-05');
		expect(dateLabel('2026-11-19')).toBe('2026-11-19（木）');
		expect(dateLabel('zzz')).toBe('zzz');
		expect(dailyReportQuery(3, '2026-11-19')).toBe('?group=3&date=2026-11-19');
	});
});

describe('dailyReportRequestFor', () => {
	it('日付とタグから引数を作る（8 本まで）', () => {
		const req = dailyReportRequestFor('2026-11-19', [1, 2, 3, 4, 5, 6, 7, 8, 9]);
		expect(req?.tagIds).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
		expect(req?.fromMs).toBe(new Date(2026, 10, 19).getTime());
		expect(req?.toMs).toBe(new Date(2026, 10, 20).getTime());
	});

	it('タグが無い・日付が不正なら null', () => {
		expect(dailyReportRequestFor('2026-11-19', [])).toBeNull();
		expect(dailyReportRequestFor('bad', [1])).toBeNull();
	});
});

describe('整形', () => {
	it('欠測は —、小数桁はタグの設定、読めなければ 2 桁', () => {
		expect(formatReportValue(null, 1)).toBe(DAILY_REPORT_EMPTY_CELL);
		expect(formatReportValue(Number.NaN, 1)).toBe(DAILY_REPORT_EMPTY_CELL);
		expect(formatReportValue(12.345, 1)).toBe('12.3');
		expect(formatReportValue(12.345, 0)).toBe('12');
		expect(formatReportValue(12.345, null)).toBe('12.35');
	});

	it('columnHeader は 名前 [単位]', () => {
		expect(columnHeader({ name: '温度', unit: '℃', decimals: 1 })).toBe('温度 [℃]');
		expect(columnHeader({ name: '圧力', unit: null, decimals: null })).toBe('圧力');
	});

	it('hourLabel はローカルの HH:mm', () => {
		expect(hourLabel(new Date(2026, 10, 19, 7, 0).getTime())).toBe('07:00');
	});
});

describe('buildReportTemplate', () => {
	it('タグ本数分の列を作り、1〜8 に丸める', () => {
		const t3 = buildReportTemplate(3);
		expect(t3).toContain('| 時刻 | {{ h0 }} | {{ h1 }} | {{ h2 }} |');
		expect(t3).toContain('|---|---:|---:|---:|');
		expect(t3).not.toContain('{{ h3 }}');
		expect(buildReportTemplate(0)).toContain('{{ h0 }}');
		expect(buildReportTemplate(0)).not.toContain('{{ h1 }}');
		expect(buildReportTemplate(99)).toContain('{{ h7 }}');
		expect(buildReportTemplate(99)).not.toContain('{{ h8 }}');
	});

	it('タイトルは日報、集計の表を持つ', () => {
		const t = buildReportTemplate(1);
		expect(t.startsWith('# 日報\n')).toBe(true);
		expect(t).toContain('## 1 日の集計');
		expect(t).toContain('{{#each hours}}');
		expect(t).toContain('{{#each summary}}');
	});
});

describe('buildReportData', () => {
	it('時間別は平均、集計は平均・最大・最小。欠測は —、桁はタグの設定', () => {
		const { data, tagCount } = buildReportData(report(), 'ライン A', '2026-11-19', info);
		expect(tagCount).toBe(2);
		expect(data.date).toBe('2026-11-19（木）');
		expect(data.group).toBe('ライン A');
		expect(data.h0).toBe('温度 [℃]');
		expect(data.h1).toBe('圧力');
		expect(data.hours).toHaveLength(24);
		expect(data.hours[0]).toEqual({ time: '00:00', v0: '0.0', v1: '100.00' });
		expect(data.hours[3].v0).toBe('—');
		expect(data.summary).toEqual([
			{ label: '平均', v0: '12.3', v1: '—' },
			{ label: '最大', v0: '23.0', v1: '—' },
			{ label: '最小', v0: '0.0', v1: '—' }
		]);
	});

	it('描画すると 24 行 + 集計 3 行の表になる（タグ名の | や HTML・{{ も壊さない）', () => {
		const nasty = (tagId: number) => ({
			name: tagId === 1 ? 'a|b <i>*x* {{ y }}' : '圧力',
			unit: null,
			decimals: 0
		});
		const { template, data } = buildReportData(report(), 'G|1', '2026-11-19', nasty);
		const { html, warnings } = renderReport(template, data);
		expect(warnings).toEqual([]);
		expect(html).toContain('日報');
		expect(html).toContain('a|b &lt;i&gt;*x* {{ y }}');
		expect(html).not.toContain('<i>');
		// 表 2 つ: 見出し行 + 24 行、見出し行 + 3 行。
		expect((html.match(/<tr>/g) ?? []).length).toBe(1 + 24 + 1 + 3);
		// 列は 1 + 2。
		const firstRow = html.slice(html.indexOf('<tr>'), html.indexOf('</tr>'));
		expect((firstRow.match(/<th/g) ?? []).length).toBe(3);
		expect(html).toContain('>—<');
	});
});

describe('isEmptyReport', () => {
	it('全部欠測なら true', () => {
		expect(isEmptyReport(report())).toBe(false);
		const empty = report({
			hours: [{ tMs: 0, values: [null, null] }],
			day: [null, null]
		});
		expect(isEmptyReport(empty)).toBe(true);
	});
});

describe('注記・エラー', () => {
	it('validation は上限の案内、それ以外は理由つき', () => {
		const v = new ProviderError({ kind: 'validation', field_errors: [] });
		expect(dailyReportErrorMessage(v)).toBe(DAILY_REPORT_LIMIT_MESSAGE);
		expect(dailyReportErrorMessage(new Error('boom'))).toContain('boom');
	});

	it('unknownTagsNote・simulationNote', () => {
		expect(unknownTagsNote([], String)).toBeNull();
		expect(unknownTagsNote([4, 5], (id) => `タグ ${id}`)).toContain('タグ 4、タグ 5');
		expect(simulationNote(report(), String)).toBeNull();
		expect(
			simulationNote(
				{
					tags: [
						{ tagId: 1, simulation: true },
						{ tagId: 2, simulation: false }
					]
				},
				(id) => `T${id}`
			)
		).toContain('（T1）');
	});

	it('simulationNote: 現在のフラグが true でも過去の記録値は出る（注記は出ないと言わない）', () => {
		// 現在のフラグ（simulation: true）と過去の統計（非 null）が併存するケース。
		const note = simulationNote({ tags: [{ tagId: 1, simulation: true }] }, (id) => `T${id}`);
		expect(note).not.toBeNull();
		expect(note).not.toContain('日報には出ません');
		expect(note).toContain('切り替える前に記録した値は表示されます');
	});
});
