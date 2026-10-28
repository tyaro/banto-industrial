import { describe, expect, it } from 'vitest';
import { formatSqliteUtc } from './sqliteTime';

// マシンのタイムゾーンに依存しないよう、期待値は同じ UTC の瞬間を
// `toLocaleString('ja-JP')`（他画面と同じ API）で組み立てる。
const expected = (iso: string) => new Date(iso).toLocaleString('ja-JP');

describe('formatSqliteUtc', () => {
	it('タイムゾーン表記のない SQLite 形式は UTC として読み、ローカル時刻にする', () => {
		expect(formatSqliteUtc('2026-10-19 21:48:19')).toBe(expected('2026-10-19T21:48:19Z'));
	});

	it('書式は他画面と同じ YYYY/M/D 形式（ハイフン区切りの UTC 文字列のままにしない）', () => {
		const out = formatSqliteUtc('2026-10-19 21:48:19');
		expect(out).toMatch(/^\d{4}\/\d{1,2}\/\d{1,2} /);
		expect(out).not.toContain('-');
	});

	it('Z 付き ISO はそのまま解釈する', () => {
		expect(formatSqliteUtc('2026-10-19T21:48:19Z')).toBe(expected('2026-10-19T21:48:19Z'));
	});

	it('小数秒つきも読める', () => {
		expect(formatSqliteUtc('2026-10-19 21:48:19.123')).toBe(expected('2026-10-19T21:48:19Z'));
	});

	it('読めない文字列は元のまま返す', () => {
		expect(formatSqliteUtc('not a date')).toBe('not a date');
	});
});
