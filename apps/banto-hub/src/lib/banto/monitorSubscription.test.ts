/**
 * `monitorSubscription.ts`（T18-4b）に対するユニットテスト。`monitorFilter.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import { subscriptionPatternsFor } from './monitorSubscription';
import type { PlcConnection, CollectionGroup } from './tagRegistryAdmin';

function connection(id: number, name: string): PlcConnection {
	return {
		id,
		name,
		protocol: 'modbus-tcp',
		host: '127.0.0.1',
		port: 502,
		unitId: 1,
		enabled: true,
		simulation: false
	} as PlcConnection;
}

function group(id: number, name: string, plcConnectionId: number): CollectionGroup {
	return { id, name, plcConnectionId, periodMs: 1000, enabled: true, defaultWritable: true };
}

describe('subscriptionPatternsFor', () => {
	const conn1 = connection(1, 'line1');
	const conn2 = connection(2, 'line2');
	const group1a = group(10, 'fast', 1);
	const group1b = group(11, 'slow', 1);
	const group2a = group(20, 'onlyGroup', 2);
	const connections = [conn1, conn2];
	const groups = [group1a, group1b, group2a];

	it('all: 常に ["*"]', () => {
		expect(subscriptionPatternsFor({ type: 'all' }, connections, groups)).toEqual(['*']);
	});

	it('group: 接続名.グループ名.* を1件返す', () => {
		const result = subscriptionPatternsFor({ type: 'group', id: 10 }, connections, groups);
		expect(result).toEqual(['line1.fast.*']);
	});

	it('group: 別のグループでも正しい接続名で組み立てる', () => {
		const result = subscriptionPatternsFor({ type: 'group', id: 20 }, connections, groups);
		expect(result).toEqual(['line2.onlyGroup.*']);
	});

	it('group: グループが見つからなければ ["*"] にフォールバックする', () => {
		const result = subscriptionPatternsFor({ type: 'group', id: 999 }, connections, groups);
		expect(result).toEqual(['*']);
	});

	it('group: グループは見つかるが所属接続が見つからなければ ["*"] にフォールバックする', () => {
		const orphanGroup = group(30, 'orphan', 999);
		const result = subscriptionPatternsFor({ type: 'group', id: 30 }, connections, [
			...groups,
			orphanGroup
		]);
		expect(result).toEqual(['*']);
	});

	it('connection: 同一接続内の複数グループを列挙する', () => {
		const result = subscriptionPatternsFor({ type: 'connection', id: 1 }, connections, groups);
		expect(result.sort()).toEqual(['line1.fast.*', 'line1.slow.*'].sort());
	});

	it('connection: グループが1件だけならその1件を返す', () => {
		const result = subscriptionPatternsFor({ type: 'connection', id: 2 }, connections, groups);
		expect(result).toEqual(['line2.onlyGroup.*']);
	});

	it('connection: 接続が見つからなければ ["*"] にフォールバックする', () => {
		const result = subscriptionPatternsFor({ type: 'connection', id: 999 }, connections, groups);
		expect(result).toEqual(['*']);
	});

	it('connection: 接続は見つかるがグループが0件なら ["*"] にフォールバックする（空配列は絶対に返さない）', () => {
		const emptyGroups: CollectionGroup[] = [];
		const result = subscriptionPatternsFor({ type: 'connection', id: 1 }, connections, emptyGroups);
		expect(result).toEqual(['*']);
	});

	it('空配列を返すケースは存在しない（全ての分岐で非空を返す）', () => {
		const cases: Array<() => string[]> = [
			() => subscriptionPatternsFor({ type: 'all' }, [], []),
			() => subscriptionPatternsFor({ type: 'group', id: 1 }, [], []),
			() => subscriptionPatternsFor({ type: 'connection', id: 1 }, [], [])
		];
		for (const run of cases) {
			expect(run().length).toBeGreaterThan(0);
		}
	});
});
