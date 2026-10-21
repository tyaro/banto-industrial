/**
 * `bridgeLoad.ts` のユニットテスト（R2a-2e、docs/banto-hub-ingest-design.md §7.1）。
 *
 * 表の値は `bridgeLoad.fixtures.json` にあり、Rust の
 * `apps/banto-hub/core/src/ingest/bridge_load_fixtures.rs` が同じ fixtures を
 * `banto_tags::bridge_load` に掛けて同じ `expected` になることを確かめている -
 * 両方が通れば、画面の見積もりとサーバーの登録時の判定は同じ値を出す。
 */
import { describe, expect, it } from 'vitest';
import fixtures from './bridgeLoad.fixtures.json';
import {
	PUSH_LIMITS,
	REGISTRATION_ITEMS_PER_SEC,
	REGISTRATION_MESSAGES_PER_SEC,
	bridgeLoad,
	formatRatePerSec,
	groupThousands,
	type BridgeLoadQueryInput
} from './bridgeLoad';

type KeysSpec =
	| string[]
	| {
			count: number;
			template?: string;
			padStart?: number;
			padChar?: string;
			repeat?: string;
			times?: number;
	  };

/** fixtures の `keys` からキーの並びを作る（Rust の `expand_keys` と同じ規則）。 */
function expandKeys(spec: KeysSpec): string[] {
	if (Array.isArray(spec)) return spec;
	if (spec.repeat !== undefined) {
		return Array.from({ length: spec.count }, () => spec.repeat!.repeat(spec.times ?? 0));
	}
	const template = spec.template ?? '';
	return Array.from({ length: spec.count }, (_, i) => {
		const key = template.replaceAll('{i}', String(i));
		return spec.padStart === undefined ? key : key.padStart(spec.padStart, spec.padChar ?? ' ');
	});
}

interface FixtureCase {
	name: string;
	queries: Array<{ periodMs: number; keys: KeysSpec }>;
	expected: unknown;
}

describe('bridgeLoad は Rust の bridge_load と同じ値を出す（共有の fixtures）', () => {
	const cases = (fixtures as { cases: FixtureCase[] }).cases;

	it('fixtures が読めている', () => {
		expect(cases.length).toBeGreaterThanOrEqual(10);
		for (const c of cases) expect(c.expected).not.toBeNull();
	});

	for (const c of cases) {
		it(c.name, () => {
			const input: BridgeLoadQueryInput[] = c.queries.map((q) => ({
				periodMs: q.periodMs,
				keys: expandKeys(q.keys)
			}));
			expect(bridgeLoad(input)).toEqual(c.expected);
		});
	}
});

describe('定数と書き方', () => {
	it('上限の 8 割は 16 メッセージ・4,000 件', () => {
		expect(REGISTRATION_MESSAGES_PER_SEC).toBe(16);
		expect(REGISTRATION_ITEMS_PER_SEC).toBe(4000);
		expect(PUSH_LIMITS.maxWsMessageBytes).toBe(262144);
		expect(PUSH_LIMITS.maxStatusDetailBytes).toBe(1024);
	});

	it('1 秒あたりの表示は小数 1 桁まで切り上げ・3 桁区切り', () => {
		expect(formatRatePerSec(1)).toBe('0.1');
		expect(formatRatePerSec(16 * 60)).toBe('16');
		expect(formatRatePerSec(165 * 6)).toBe('16.5');
		expect(formatRatePerSec(4000 * 60)).toBe('4,000');
		expect(groupThousands(1234567)).toBe('1,234,567');
		expect(groupThousands(0)).toBe('0');
	});
});
