/**
 * `tagOnboarding.ts`（T18-2d、docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A）
 * のユニットテスト。`tagFormCarry.test.ts`/`tagDeleteImpact.test.ts` と同じ
 * スタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';
import type { ConnectionStatusEntry, ValueEntry } from './hubStatus';
import {
	collectionGroupsHref,
	computeOnboardingSteps,
	connectionAwaitingGroup,
	groupAwaitingTag,
	isOnboardingComplete,
	nextOnboardingStep,
	resolveGroupIdFromTreeSelection,
	resolvePresetConnectionId,
	resolvePresetGroupId,
	tagsHref,
	type OnboardingSnapshot
} from './tagOnboarding';

function connection(overrides: Partial<PlcConnection> = {}): PlcConnection {
	return {
		id: 1,
		name: 'plc1',
		protocol: 'modbus-tcp',
		host: '127.0.0.1',
		port: 502,
		unitId: 1,
		enabled: true,
		simulation: true,
		wordOrder: 'low_high',
		...overrides
	};
}

function group(overrides: Partial<CollectionGroup> = {}): CollectionGroup {
	return {
		id: 1,
		name: 'group1',
		plcConnectionId: 1,
		periodMs: 1000,
		enabled: true,
		...overrides
	};
}

function tag(overrides: Partial<Tag> = {}): Tag {
	return {
		id: 1,
		name: 'tag1',
		collectionGroupId: 1,
		address: 'D100',
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

function connectionStatus(overrides: Partial<ConnectionStatusEntry> = {}): ConnectionStatusEntry {
	return { name: 'plc1', id: 1, status: 'connected', attempt: null, ...overrides };
}

function valueEntry(overrides: Partial<ValueEntry> = {}): ValueEntry {
	return { tag: 'plc1.group1.tag1', v: 1, q: 'good', t: 1, ...overrides };
}

const CALC = connection({ id: 900, name: 'calc', protocol: 'virtual', simulation: false });
const MEM = connection({ id: 901, name: 'mem', protocol: 'virtual', simulation: false });

function emptySnapshot(): OnboardingSnapshot {
	return { connections: [], groups: [], tags: [], connectionStatuses: [], values: [] };
}

describe('computeOnboardingSteps', () => {
	it('空環境ではすべて未完了で、それぞれの遷移先が既定のページになる', () => {
		const steps = computeOnboardingSteps(emptySnapshot());
		expect(steps).toHaveLength(5);
		expect(steps.every((s) => !s.done)).toBe(true);
		expect(steps.map((s) => s.id)).toEqual([
			'connection',
			'connectionTest',
			'group',
			'tag',
			'simValue'
		]);
		expect(steps.find((s) => s.id === 'group')?.href).toBe('/collection-groups');
		expect(steps.find((s) => s.id === 'tag')?.href).toBe('/tags');
	});

	it('virtual接続（calc/mem）だけでは「PLC接続の作成」を完了扱いにしない', () => {
		const steps = computeOnboardingSteps({
			...emptySnapshot(),
			connections: [CALC, MEM]
		});
		expect(steps.find((s) => s.id === 'connection')?.done).toBe(false);
	});

	it('実接続を作成すると「PLC接続の作成」が完了し、次工程の href に接続 ID が付く', () => {
		const conn = connection({ id: 1 });
		const steps = computeOnboardingSteps({ ...emptySnapshot(), connections: [conn] });
		expect(steps.find((s) => s.id === 'connection')?.done).toBe(true);
		expect(steps.find((s) => s.id === 'group')?.href).toBe('/collection-groups?connectionId=1');
	});

	it('接続テストは status.connections の connected 状態から判定する（テストボタンの記録ではない）', () => {
		const conn = connection({ id: 1 });
		const notConnected = computeOnboardingSteps({
			...emptySnapshot(),
			connections: [conn],
			connectionStatuses: [connectionStatus({ id: 1, status: 'reconnecting' })]
		});
		expect(notConnected.find((s) => s.id === 'connectionTest')?.done).toBe(false);

		const connected = computeOnboardingSteps({
			...emptySnapshot(),
			connections: [conn],
			connectionStatuses: [connectionStatus({ id: 1, status: 'connected' })]
		});
		expect(connected.find((s) => s.id === 'connectionTest')?.done).toBe(true);
	});

	it('virtual接続配下のグループは「収集グループの作成」に数えない', () => {
		const steps = computeOnboardingSteps({
			...emptySnapshot(),
			connections: [CALC],
			groups: [group({ id: 1, plcConnectionId: CALC.id })]
		});
		expect(steps.find((s) => s.id === 'group')?.done).toBe(false);
	});

	it('実収集グループがあれば完了し、タグ工程の href にグループ ID が付く', () => {
		const conn = connection({ id: 1 });
		const g = group({ id: 10, plcConnectionId: 1 });
		const steps = computeOnboardingSteps({ ...emptySnapshot(), connections: [conn], groups: [g] });
		expect(steps.find((s) => s.id === 'group')?.done).toBe(true);
		expect(steps.find((s) => s.id === 'tag')?.href).toBe('/tags?groupId=10');
	});

	it('computed/internal タグだけでは「タグの登録」を完了扱いにしない', () => {
		const steps = computeOnboardingSteps({
			...emptySnapshot(),
			tags: [tag({ tagKind: 'computed' }), tag({ id: 2, tagKind: 'internal' })]
		});
		expect(steps.find((s) => s.id === 'tag')?.done).toBe(false);
	});

	it('plc タグが1件あれば「タグの登録」が完了する', () => {
		const steps = computeOnboardingSteps({ ...emptySnapshot(), tags: [tag({ tagKind: 'plc' })] });
		expect(steps.find((s) => s.id === 'tag')?.done).toBe(true);
	});

	it('q: good の値が1件でもあれば「SIM値の確認」が完了する', () => {
		const notGood = computeOnboardingSteps({
			...emptySnapshot(),
			values: [valueEntry({ q: 'bad' }), valueEntry({ q: 'stale' })]
		});
		expect(notGood.find((s) => s.id === 'simValue')?.done).toBe(false);

		const good = computeOnboardingSteps({
			...emptySnapshot(),
			values: [valueEntry({ q: 'good' })]
		});
		expect(good.find((s) => s.id === 'simValue')?.done).toBe(true);
	});

	it('すべて満たすとどの工程も done になる', () => {
		const conn = connection({ id: 1 });
		const g = group({ id: 10, plcConnectionId: 1 });
		const t = tag({ id: 100, collectionGroupId: 10, tagKind: 'plc' });
		const steps = computeOnboardingSteps({
			connections: [conn],
			groups: [g],
			tags: [t],
			connectionStatuses: [connectionStatus({ id: 1, status: 'connected' })],
			values: [valueEntry({ q: 'good' })]
		});
		expect(steps.every((s) => s.done)).toBe(true);
	});
});

describe('nextOnboardingStep / isOnboardingComplete', () => {
	it('未完了の先頭工程を返し、全完了なら null', () => {
		const steps = computeOnboardingSteps(emptySnapshot());
		expect(nextOnboardingStep(steps)?.id).toBe('connection');
		expect(isOnboardingComplete(steps)).toBe(false);

		const allDone = steps.map((s) => ({ ...s, done: true }));
		expect(nextOnboardingStep(allDone)).toBeNull();
		expect(isOnboardingComplete(allDone)).toBe(true);
	});

	it('空配列は未完了扱い（データ未取得と区別する）', () => {
		expect(isOnboardingComplete([])).toBe(false);
	});
});

describe('connectionAwaitingGroup / groupAwaitingTag', () => {
	it('グループを持たない接続を優先して返す', () => {
		const withGroup = connection({ id: 1 });
		const withoutGroup = connection({ id: 2 });
		const target = connectionAwaitingGroup(
			[withGroup, withoutGroup],
			[group({ plcConnectionId: 1 })]
		);
		expect(target?.id).toBe(2);
	});

	it('全接続にグループがあれば先頭接続にフォールバックする', () => {
		const conn = connection({ id: 1 });
		const target = connectionAwaitingGroup([conn], [group({ plcConnectionId: 1 })]);
		expect(target?.id).toBe(1);
	});

	it('実接続が無ければ null', () => {
		expect(connectionAwaitingGroup([CALC, MEM], [])).toBeNull();
	});

	it('plc タグを持たないグループを優先して返す', () => {
		const conn = connection({ id: 1 });
		const withTag = group({ id: 10, plcConnectionId: 1 });
		const withoutTag = group({ id: 11, plcConnectionId: 1 });
		const target = groupAwaitingTag(
			[withTag, withoutTag],
			[conn],
			[tag({ collectionGroupId: 10, tagKind: 'plc' })]
		);
		expect(target?.id).toBe(11);
	});
});

describe('collectionGroupsHref / tagsHref', () => {
	it('id が無ければクエリ無しの既定パス', () => {
		expect(collectionGroupsHref(null)).toBe('/collection-groups');
		expect(tagsHref(null)).toBe('/tags');
	});

	it('id があればプリセット用クエリを付ける', () => {
		expect(collectionGroupsHref(5)).toBe('/collection-groups?connectionId=5');
		expect(tagsHref(7)).toBe('/tags?groupId=7');
	});
});

describe('resolvePresetConnectionId', () => {
	const connections = [connection({ id: 1 }), CALC];

	it('null・非数値・存在しない ID はすべて null', () => {
		expect(resolvePresetConnectionId(null, connections)).toBeNull();
		expect(resolvePresetConnectionId('abc', connections)).toBeNull();
		expect(resolvePresetConnectionId('999', connections)).toBeNull();
	});

	it('virtual 接続（calc/mem）は null', () => {
		expect(resolvePresetConnectionId(String(CALC.id), connections)).toBeNull();
	});

	it('存在する実接続の ID はそのまま返す', () => {
		expect(resolvePresetConnectionId('1', connections)).toBe(1);
	});
});

describe('resolvePresetGroupId', () => {
	const connections = [connection({ id: 1 }), CALC];
	const groups = [
		group({ id: 10, plcConnectionId: 1 }),
		group({ id: 20, plcConnectionId: CALC.id })
	];

	it('null・非数値・存在しないグループは null', () => {
		expect(resolvePresetGroupId(null, groups, connections)).toBeNull();
		expect(resolvePresetGroupId('xyz', groups, connections)).toBeNull();
		expect(resolvePresetGroupId('999', groups, connections)).toBeNull();
	});

	it('virtual接続配下のグループ（calc/mem用）は null（選ばせない）', () => {
		expect(resolvePresetGroupId('20', groups, connections)).toBeNull();
	});

	it('実接続配下のグループはそのまま返す', () => {
		expect(resolvePresetGroupId('10', groups, connections)).toBe(10);
	});
});

describe('resolveGroupIdFromTreeSelection', () => {
	const connections = [connection({ id: 1 }), CALC];
	const groups = [
		group({ id: 10, plcConnectionId: 1 }),
		group({ id: 20, plcConnectionId: CALC.id })
	];

	it('"all" 選択は null', () => {
		expect(resolveGroupIdFromTreeSelection({ type: 'all' }, groups, connections)).toBeNull();
	});

	it('接続選択は null（グループが一意に決まらない）', () => {
		expect(
			resolveGroupIdFromTreeSelection({ type: 'connection', id: 1 }, groups, connections)
		).toBeNull();
	});

	it('グループ選択はそのグループ ID を返す', () => {
		expect(resolveGroupIdFromTreeSelection({ type: 'group', id: 10 }, groups, connections)).toBe(
			10
		);
	});

	it('calc/mem 配下のグループ選択は null', () => {
		expect(
			resolveGroupIdFromTreeSelection({ type: 'group', id: 20 }, groups, connections)
		).toBeNull();
	});
});
