import { describe, expect, it } from 'vitest';
import type { CollectionGroup, PlcConnection } from '#lib/banto/tagRegistryAdmin.js';
import {
	buildRecordTreeNodes,
	connectionAccessibleName,
	formatPeriodMs,
	groupAccessibleName,
	isNavigationKey,
	recordTargetNodeId,
	recordTargetOf
} from './recordTree.js';

function connection(overrides: Partial<PlcConnection> & { id: number }): PlcConnection {
	return {
		name: `PLC${overrides.id}`,
		protocol: 'slmp',
		enabled: true,
		settings: {},
		simulation: false,
		...overrides
	} as PlcConnection;
}

function group(overrides: Partial<CollectionGroup> & { id: number }): CollectionGroup {
	return {
		name: `G${overrides.id}`,
		plcConnectionId: 1,
		periodMs: 1000,
		enabled: true,
		...overrides
	};
}

describe('buildRecordTreeNodes', () => {
	it('「すべて」の下に接続先、その下に収集グループ（件数と周期）を並べる', () => {
		const nodes = buildRecordTreeNodes(
			[connection({ id: 1 }), connection({ id: 2, name: 'HubA', protocol: 'hub' })],
			[group({ id: 10 }), group({ id: 11, periodMs: 100 }), group({ id: 20, plcConnectionId: 2 })],
			[{ collectionGroupId: 10 }, { collectionGroupId: 10 }, { collectionGroupId: 20 }]
		);
		expect(nodes.map((n) => n.id)).toEqual(['all', 'conn:1', 'conn:2']);
		expect(nodes[0]).toMatchObject({ label: 'すべて', data: { kind: 'all' } });
		expect(nodes[0]!.children).toBeUndefined();
		expect(nodes[1]!.label).toBe('PLC1 PLC 直結');
		expect(nodes[1]!.children!.map((n) => [n.id, n.label])).toEqual([
			['group:10', 'G10 (2) 1s'],
			['group:11', 'G11 (0) 100ms']
		]);
		expect(nodes[2]!.label).toBe('HubA Hub 経由');
		expect(nodes[2]!.children!.map((n) => n.label)).toEqual(['G20 (1) 1s']);
		expect(nodes[1]!.children![0]!.data).toMatchObject({
			kind: 'group',
			group: { id: 10 },
			connection: { id: 1 }
		});
	});

	it('収集グループが 0 件の接続先にも空の children を渡す（未登録の案内行）', () => {
		const nodes = buildRecordTreeNodes([connection({ id: 3 })], [], []);
		expect(nodes[1]!.children).toEqual([]);
	});
});

describe('行の名前と印', () => {
	it('無効・シミュレーションの印を付ける（Hub 経由にシミュレーションは付けない）', () => {
		expect(connectionAccessibleName(connection({ id: 1, enabled: false, simulation: true }))).toBe(
			'PLC1 PLC 直結 無効 シミュレーション'
		);
		expect(connectionAccessibleName(connection({ id: 2, protocol: 'hub', simulation: true }))).toBe(
			'PLC2 Hub 経由'
		);
		expect(groupAccessibleName(group({ id: 5, enabled: false, periodMs: 500 }), 3)).toBe(
			'G5 (3) 500ms 無効'
		);
	});

	it('周期は 1 秒以上を秒で書く', () => {
		expect(formatPeriodMs(100)).toBe('100ms');
		expect(formatPeriodMs(1000)).toBe('1s');
		expect(formatPeriodMs(5000)).toBe('5s');
	});
});

describe('対象とノードの id', () => {
	it('行き来できる', () => {
		expect(recordTargetNodeId({ type: 'all' })).toBe('all');
		expect(recordTargetNodeId({ type: 'connection', id: 4 })).toBe('conn:4');
		expect(recordTargetNodeId({ type: 'group', id: 7 })).toBe('group:7');
		expect(recordTargetOf({ kind: 'all' })).toEqual({ type: 'all' });
		expect(recordTargetOf({ kind: 'connection', connection: connection({ id: 4 }) })).toEqual({
			type: 'connection',
			id: 4
		});
		expect(
			recordTargetOf({ kind: 'group', group: group({ id: 7 }), connection: connection({ id: 1 }) })
		).toEqual({ type: 'group', id: 7 });
	});
});

describe('isNavigationKey', () => {
	it('矢印・Home・End・PageUp/Down だけ', () => {
		for (const key of ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End']) {
			expect(isNavigationKey(key)).toBe(true);
		}
		for (const key of ['Enter', ' ', 'a', 'Escape']) expect(isNavigationKey(key)).toBe(false);
	});
});
