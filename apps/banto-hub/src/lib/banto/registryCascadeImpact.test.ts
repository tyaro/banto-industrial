/**
 * `registryCascadeImpact.ts` のユニットテスト（`tagDeleteImpact.test.ts`と
 * 同じスタイル、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	countConnectionCascadeImpact,
	countGroupCascadeImpact,
	formatConnectionDeleteConfirmMessage,
	formatGroupDeleteConfirmMessage
} from './registryCascadeImpact';
import type { CollectionGroup, Tag } from './tagRegistryAdmin';

function makeGroup(overrides: Partial<CollectionGroup>): CollectionGroup {
	return {
		id: 1,
		name: 'fast',
		plcConnectionId: 1,
		periodMs: 1000,
		enabled: true,
		defaultWritable: true,
		...overrides
	};
}

function makeTag(overrides: Partial<Tag>): Tag {
	return {
		id: 1,
		name: 'temp01',
		collectionGroupId: 1,
		address: '40001',
		dataType: 'f32',
		stringLength: null,
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit: null,
		decimals: 0,
		thresholdH: null,
		thresholdHh: null,
		thresholdL: null,
		thresholdLl: null,
		enabled: true,
		writable: false,
		tagKind: 'plc',
		expression: null,
		retain: false,
		revision: 1,
		...overrides
	};
}

describe('countConnectionCascadeImpact', () => {
	it('counts only the groups/tags under the target connection', () => {
		const groups = [
			makeGroup({ id: 1, plcConnectionId: 1 }),
			makeGroup({ id: 2, plcConnectionId: 1 }),
			makeGroup({ id: 3, plcConnectionId: 2 })
		];
		const tags = [
			makeTag({ id: 1, collectionGroupId: 1 }),
			makeTag({ id: 2, collectionGroupId: 1 }),
			makeTag({ id: 3, collectionGroupId: 2 }),
			makeTag({ id: 4, collectionGroupId: 3 })
		];
		expect(countConnectionCascadeImpact(1, groups, tags)).toEqual({ groups: 2, tags: 3 });
		expect(countConnectionCascadeImpact(2, groups, tags)).toEqual({ groups: 1, tags: 1 });
	});

	it('is zero for a connection with no groups', () => {
		expect(countConnectionCascadeImpact(999, [], [])).toEqual({ groups: 0, tags: 0 });
	});
});

describe('countGroupCascadeImpact', () => {
	it('counts only the tags under the target group', () => {
		const tags = [
			makeTag({ id: 1, collectionGroupId: 1 }),
			makeTag({ id: 2, collectionGroupId: 1 }),
			makeTag({ id: 3, collectionGroupId: 2 })
		];
		expect(countGroupCascadeImpact(1, tags)).toEqual({ tags: 2 });
		expect(countGroupCascadeImpact(2, tags)).toEqual({ tags: 1 });
	});

	it('is zero for a group with no tags', () => {
		expect(countGroupCascadeImpact(999, [])).toEqual({ tags: 0 });
	});
});

describe('formatConnectionDeleteConfirmMessage', () => {
	it('always names the connection and states history is kept', () => {
		const message = formatConnectionDeleteConfirmMessage('line1', { groups: 0, tags: 0 });
		expect(message).toContain('line1 を削除しますか？');
		expect(message).toContain('記録済みの履歴（収集データ）は削除されません。');
		expect(message).not.toContain('件も一緒に削除されます');
	});

	it('states the exact group/tag counts when there are children', () => {
		const message = formatConnectionDeleteConfirmMessage('line1', { groups: 2, tags: 5 });
		expect(message).toContain('収集グループ 2件とタグ 5件も一緒に削除されます。');
		expect(message).toContain('記録済みの履歴（収集データ）は削除されません。');
	});
});

describe('formatGroupDeleteConfirmMessage', () => {
	it('always names the group and states history is kept', () => {
		const message = formatGroupDeleteConfirmMessage('fast', { tags: 0 });
		expect(message).toContain('fast を削除しますか？');
		expect(message).toContain('記録済みの履歴（収集データ）は削除されません。');
		expect(message).not.toContain('件も一緒に削除されます');
	});

	it('states the exact tag count when there are tags', () => {
		const message = formatGroupDeleteConfirmMessage('fast', { tags: 3 });
		expect(message).toContain('タグ 3件も一緒に削除されます。');
		expect(message).toContain('記録済みの履歴（収集データ）は削除されません。');
	});
});
