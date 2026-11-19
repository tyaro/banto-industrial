import { describe, expect, it } from 'vitest';
import { HISTORY_MAX_WINDOW_MS, type CollectExport } from '../banto/collectAdmin';
import {
	buildExportCsv,
	exportFileBase,
	exportFileName,
	exportRequestFor,
	exportTimeLabel,
	runExport
} from './exportCsv';

const BOM = '﻿';

function result(partial: Partial<CollectExport>): CollectExport {
	return {
		fromMs: 0,
		toMs: 0,
		mode: 'interval',
		intervalMs: 60_000,
		stat: 'avg',
		tags: [],
		unknownTagIds: [],
		rows: [],
		...partial
	};
}

const local = (y: number, mo: number, d: number, h: number, mi: number, s: number, ms = 0) =>
	new Date(y, mo - 1, d, h, mi, s, ms).getTime();

describe('exportRequestFor', () => {
	it('raw は期間をそのまま、間隔・統計なし', () => {
		expect(exportRequestFor({ fromMs: 12_345, toMs: 99_999 }, [1, 2], { mode: 'raw' })).toEqual({
			tagIds: [1, 2],
			fromMs: 12_345,
			toMs: 99_999,
			mode: 'raw'
		});
	});

	it('interval も表示の期間をそのまま使う（開始が間隔の倍数でなくても動かさない）', () => {
		const fromMs = local(2026, 11, 19, 10, 0, 30);
		const toMs = local(2026, 11, 19, 12, 0, 0);
		const req = exportRequestFor({ fromMs, toMs }, [1], {
			mode: 'interval',
			intervalMs: 600_000,
			stat: 'max'
		});
		expect(req).toEqual({
			tagIds: [1],
			fromMs,
			toMs,
			mode: 'interval',
			intervalMs: 600_000,
			stat: 'max'
		});
	});

	it('上限ちょうどの 31 日でも、開始・終了とも動かさない', () => {
		const toMs = 100 * 3_600_000 + 1_000;
		const fromMs = toMs - HISTORY_MAX_WINDOW_MS;
		for (const intervalMs of [60_000, 600_000, 3_600_000]) {
			const req = exportRequestFor({ fromMs, toMs }, [1], {
				mode: 'interval',
				intervalMs,
				stat: 'avg'
			});
			expect(req.fromMs).toBe(fromMs);
			expect(req.toMs).toBe(toMs);
			expect(req.toMs - req.fromMs).toBe(HISTORY_MAX_WINDOW_MS);
		}
	});
});

describe('runExport', () => {
	it('読み出し → 保存の順に進む', async () => {
		const calls: string[] = [];
		const outcome = await runExport({
			signal: new AbortController().signal,
			fetch: async () => {
				calls.push('fetch');
				return 'data';
			},
			save: async (data) => {
				calls.push(`save:${data}`);
				return 'saved';
			}
		});
		expect(calls).toEqual(['fetch', 'save:data']);
		expect(outcome).toEqual({ kind: 'done', result: 'saved' });
	});

	it('読み出しの途中で中止したら、結果が返っても保存しない', async () => {
		const controller = new AbortController();
		let resolveFetch: (v: string) => void = () => {};
		let saved = false;
		const pending = runExport({
			signal: controller.signal,
			fetch: () => new Promise<string>((r) => (resolveFetch = r)),
			save: async () => {
				saved = true;
				return 'saved';
			}
		});
		controller.abort();
		resolveFetch('late');
		expect(await pending).toEqual({ kind: 'cancelled' });
		expect(saved).toBe(false);
	});

	it('読み出しが失敗しても中止済みなら cancelled（エラーにしない）', async () => {
		const controller = new AbortController();
		const pending = runExport({
			signal: controller.signal,
			fetch: async () => {
				controller.abort();
				throw new Error('aborted');
			},
			save: async () => 'saved'
		});
		expect(await pending).toEqual({ kind: 'cancelled' });
	});

	it('保存の途中で中止されたら cancelled', async () => {
		const controller = new AbortController();
		const outcome = await runExport({
			signal: controller.signal,
			fetch: async () => 'data',
			save: async () => {
				controller.abort();
				return 'saved';
			}
		});
		expect(outcome).toEqual({ kind: 'cancelled' });
	});
});

describe('exportTimeLabel', () => {
	it('ローカル時刻。raw はミリ秒付き、interval は秒まで', () => {
		const t = local(2026, 11, 9, 3, 4, 5, 6);
		expect(exportTimeLabel(t, true)).toBe('2026/11/09 03:04:05.006');
		expect(exportTimeLabel(t, false)).toBe('2026/11/09 03:04:05');
	});
});

describe('buildExportCsv', () => {
	const info = (tagId: number) =>
		(
			({
				1: { name: '温度', unit: '℃' },
				2: { name: '圧力' },
				3: { name: '温度', unit: '℃' },
				4: { name: '=SUM(A1)', unit: null }
			}) as Record<number, { name: string; unit?: string | null }>
		)[tagId] ?? { name: `タグ ${tagId}` };

	it('見出し: 時刻・名前 [単位]・重複は区別・数式風は無害化。BOM 付き', () => {
		const csv = buildExportCsv(
			result({ tags: [1, 2, 3, 4].map((tagId) => ({ tagId, simulation: false })) }),
			info
		);
		expect(csv.startsWith(BOM)).toBe(true);
		expect(csv.slice(1)).toBe("時刻,温度 [℃],圧力,温度 [℃] (#3),'=SUM(A1)");
	});

	it('raw はミリ秒付きの時刻、null は空欄、数値はそのまま', () => {
		const t = local(2026, 1, 2, 3, 4, 5, 120);
		const csv = buildExportCsv(
			result({
				mode: 'raw',
				intervalMs: null,
				stat: null,
				tags: [
					{ tagId: 1, simulation: false },
					{ tagId: 2, simulation: false }
				],
				rows: [
					{ tMs: t, values: [1.5, null] },
					{ tMs: t + 1000, values: [-2, 0] }
				]
			}),
			info
		);
		expect(csv.slice(1).split('\r\n')).toEqual([
			'時刻,温度 [℃],圧力',
			'2026/01/02 03:04:05.120,1.5,',
			'2026/01/02 03:04:06.120,-2,0'
		]);
	});

	it('interval は秒までの時刻。未知のタグは列にならない', () => {
		const t = local(2026, 1, 2, 3, 4, 0);
		const csv = buildExportCsv(
			result({
				tags: [{ tagId: 2, simulation: false }],
				unknownTagIds: [9],
				rows: [{ tMs: t, values: [3] }]
			}),
			info
		);
		expect(csv.slice(1).split('\r\n')).toEqual(['時刻,圧力', '2026/01/02 03:04:00,3']);
	});
});

describe('exportFileBase / exportFileName', () => {
	it('raw と interval の名前', () => {
		expect(exportFileBase('ライン1', { mode: 'raw' })).toBe('ライン1-raw');
		expect(exportFileBase('ライン1', { mode: 'interval', intervalMs: 600_000, stat: 'avg' })).toBe(
			'ライン1-10分-平均'
		);
		expect(exportFileBase('A', { mode: 'interval', intervalMs: 3_600_000, stat: 'min' })).toBe(
			'A-1時間-最小'
		);
	});

	it('使えない文字と制御文字は _ にし、空なら export', () => {
		expect(exportFileBase('a/b\\c:d*e?f"g<h>i|j\u0001k', { mode: 'raw' })).toBe(
			'a_b_c_d_e_f_g_h_i_j_k-raw'
		);
		expect(exportFileBase('  ', { mode: 'raw' })).toBe('export-raw');
	});

	it('最終名は日時と .csv を足す', () => {
		expect(exportFileName('G', { mode: 'raw' }, new Date(2026, 10, 19, 9, 5))).toBe(
			'G-raw-20261119-0905.csv'
		);
	});
});
