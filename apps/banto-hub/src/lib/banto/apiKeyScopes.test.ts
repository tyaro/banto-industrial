/**
 * R2a-2b（docs/banto-hub-ingest-design.md §5・§11）: `apiKeyScopes.ts` の
 * `ingest:#{id}` の組み立て・読み取りと、一覧の表示（今の名前の解決・削除
 * 済み）のテスト。文法の表は `apps/banto-hub/core/src/api_keys.rs` の
 * `ingest_scope_grammar_table` と同じ並び、文言は `mcp.rs` の
 * `ingest_targets` のテストと同じ。
 */
import { describe, expect, it } from 'vitest';
import {
	describeApiKeyScope,
	ingestScope,
	parseIngestScope,
	pushConnectionOptions,
	reconcileIngestSelection,
	type ScopeConnection
} from './apiKeyScopes';

const CONNECTIONS: ScopeConnection[] = [
	{ id: 3, name: 'line1', protocol: 'slmp' },
	{ id: 12, name: 'Bridge1', protocol: 'push' },
	{ id: 14, name: 'Bridge2', protocol: 'push' },
	{ id: 20, name: 'db1', protocol: 'postgres' }
];

describe('parseIngestScope', () => {
	it('accepts only the canonical ingest:#{id}', () => {
		expect(parseIngestScope('ingest:#12')).toBe(12);
		expect(parseIngestScope('ingest:#1')).toBe(1);
		expect(parseIngestScope(ingestScope(4096))).toBe(4096);
	});

	it.each([
		'ingest:',
		'ingest:#',
		'ingest:#0',
		'ingest:#012',
		'ingest:#-1',
		'ingest:#+1',
		'ingest:# 12',
		'ingest:#12 ',
		'ingest:#12x',
		'ingest:#*',
		'ingest:Bridge1',
		'ingest:#１２',
		'ingest:#9223372036854775808',
		'read',
		'admin'
	])('rejects %j', (scope) => {
		expect(parseIngestScope(scope)).toBeNull();
	});
});

describe('describeApiKeyScope', () => {
	it('shows the current name of a push connection', () => {
		expect(describeApiKeyScope('ingest:#12', CONNECTIONS)).toBe('取り込み: Bridge1（#12）');
	});

	it('follows a rename (the id stays, the name is looked up now)', () => {
		const renamed = CONNECTIONS.map((c) => (c.id === 12 ? { ...c, name: 'Bridge1-new' } : c));
		expect(describeApiKeyScope('ingest:#12', renamed)).toBe('取り込み: Bridge1-new（#12）');
	});

	it('marks a missing connection as deleted', () => {
		expect(describeApiKeyScope('ingest:#99', CONNECTIONS)).toBe('取り込み: （削除済み #99）');
		expect(describeApiKeyScope('ingest:#12', [])).toBe('取り込み: （削除済み #12）');
	});

	it('says so when the connection is no longer push', () => {
		expect(describeApiKeyScope('ingest:#3', CONNECTIONS)).toBe(
			'取り込み: line1（#3、push 接続ではありません）'
		);
	});

	it('does not claim deleted when the connection list could not be read', () => {
		expect(describeApiKeyScope('ingest:#12', null)).toBe(
			'取り込み: #12（接続の一覧を読めませんでした）'
		);
	});

	it('leaves other scopes verbatim', () => {
		for (const scope of ['read', 'admin', 'read:line1.fast.*', 'write:line1.fast.temp01']) {
			expect(describeApiKeyScope(scope, CONNECTIONS)).toBe(scope);
			expect(describeApiKeyScope(scope, null)).toBe(scope);
		}
	});
});

describe('pushConnectionOptions', () => {
	it('offers only push connections in their original order', () => {
		expect(pushConnectionOptions(CONNECTIONS).map((c) => c.id)).toEqual([12, 14]);
		expect(pushConnectionOptions([])).toEqual([]);
	});
});

describe('reconcileIngestSelection', () => {
	it('returns [] when the connection list could not be read', () => {
		expect(reconcileIngestSelection([12, 14], null)).toEqual([]);
	});

	it('drops ids whose connection was deleted', () => {
		expect(reconcileIngestSelection([12, 99], CONNECTIONS)).toEqual([12]);
	});

	it('drops ids that are not push connections', () => {
		expect(reconcileIngestSelection([3, 12, 20], CONNECTIONS)).toEqual([12]);
	});

	it('keeps present push ids in the selected order', () => {
		expect(reconcileIngestSelection([14, 12], CONNECTIONS)).toEqual([14, 12]);
		expect(reconcileIngestSelection([], CONNECTIONS)).toEqual([]);
	});

	it('選択 → 別キーの失効 → 接続一覧の取得失敗 → 発行 で ingest スコープが付かない', () => {
		const selected = [12];
		// 取得失敗の reload（connections = null）で選択が空に整えられる。
		const reconciled = reconcileIngestSelection(selected, null);
		const scopes = reconciled.map(ingestScope);
		expect(reconciled).toEqual([]);
		expect(scopes).not.toContain('ingest:#12');
	});
});
