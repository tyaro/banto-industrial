/**
 * T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46）: `systemInfoFormat.ts`
 * の純関数（`formatBytes`/`formatPercent`）のユニットテスト。
 */
import { describe, expect, it } from 'vitest';
import { formatBytes, formatPercent } from './systemInfoFormat';

describe('formatBytes', () => {
	it('formats zero as 0 B', () => {
		expect(formatBytes(0)).toBe('0 B');
	});

	it('keeps sub-KB values as whole bytes', () => {
		expect(formatBytes(512)).toBe('512 B');
	});

	it('formats exactly 1 KB', () => {
		expect(formatBytes(1024)).toBe('1.0 KB');
	});

	it('rounds KB values to one decimal place', () => {
		expect(formatBytes(1536)).toBe('1.5 KB');
	});

	it('formats MB values', () => {
		expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MB');
	});

	it('formats GB values', () => {
		expect(formatBytes(2.5 * 1024 * 1024 * 1024)).toBe('2.5 GB');
	});

	it('formats TB values and does not overflow past the largest unit', () => {
		expect(formatBytes(3 * 1024 * 1024 * 1024 * 1024)).toBe('3.0 TB');
		expect(formatBytes(1024 * 1024 * 1024 * 1024 * 1024)).toBe('1024.0 TB');
	});

	it('treats negative, NaN, and infinite input defensively as 0 B', () => {
		expect(formatBytes(-1)).toBe('0 B');
		expect(formatBytes(Number.NaN)).toBe('0 B');
		expect(formatBytes(Number.POSITIVE_INFINITY)).toBe('0 B');
	});
});

describe('formatPercent', () => {
	it('formats zero (plausible for the first sample - SystemInfoSampler doc comment)', () => {
		expect(formatPercent(0)).toBe('0.0%');
	});

	it('rounds to one decimal place', () => {
		expect(formatPercent(12.34)).toBe('12.3%');
		expect(formatPercent(12.36)).toBe('12.4%');
	});

	it('allows values above 100% (multi-core processes can exceed one core)', () => {
		expect(formatPercent(250)).toBe('250.0%');
	});

	it('treats negative, NaN, and infinite input defensively as 0.0%', () => {
		expect(formatPercent(-5)).toBe('0.0%');
		expect(formatPercent(Number.NaN)).toBe('0.0%');
		expect(formatPercent(Number.POSITIVE_INFINITY)).toBe('0.0%');
	});
});
