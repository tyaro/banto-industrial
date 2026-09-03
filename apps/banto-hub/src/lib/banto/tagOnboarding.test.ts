/**
 * `tagOnboarding.ts`（T18-2d、docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A）
 * のユニットテスト。`tagFormCarry.test.ts`/`tagDeleteImpact.test.ts` と同じ
 * スタイル（describe/it、依存ゼロの純関数を直接 import）。
 *
 * T19 S1-d（docs/banto-hub-t19-design.md UX-44、2026-09-03）: 初回
 * チェックリスト本体（`computeOnboardingSteps` 等）を撤去したのに合わせ、
 * その専用テスト（`computeOnboardingSteps`/`nextOnboardingStep・
 * isOnboardingComplete`/`connectionAwaitingGroup・groupAwaitingTag`/
 * `collectionGroupsHref・tagsHref` の各 describe ブロック）を削除した。
 * 残す `monitorHref`/プリセット解決系/`resolveRegistrationTarget` の
 * テストは無改変。
 */
import { describe, expect, it } from 'vitest';
import type { CollectionGroup, PlcConnection } from './tagRegistryAdmin';
import {
	monitorHref,
	resolveGroupIdFromTreeSelection,
	resolvePresetConnectionId,
	resolvePresetGroupId,
	resolveRegistrationTarget
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
		defaultWritable: true,
		...overrides
	};
}

const CALC = connection({ id: 900, name: 'calc', protocol: 'virtual', simulation: false });
const MEM = connection({ id: 901, name: 'mem', protocol: 'virtual', simulation: false });

describe('monitorHref', () => {
	it('何も指定しなければ素の /monitor', () => {
		expect(monitorHref({})).toBe('/monitor');
	});

	it('group のみ指定', () => {
		expect(monitorHref({ groupId: 5 })).toBe('/monitor?group=5');
	});

	it('connection のみ指定', () => {
		expect(monitorHref({ connectionId: 3 })).toBe('/monitor?connection=3');
	});

	it('group と connection を両方指定した場合は group が優先される', () => {
		expect(monitorHref({ groupId: 5, connectionId: 3 })).toBe('/monitor?group=5');
	});

	it('group と focus の併用', () => {
		expect(monitorHref({ groupId: 5, focus: ['plc1.group1.tag1'] })).toBe(
			'/monitor?group=5&focus=plc1.group1.tag1'
		);
	});

	it('connection と focus の併用', () => {
		expect(monitorHref({ connectionId: 3, focus: ['plc1.group1.tag1'] })).toBe(
			'/monitor?connection=3&focus=plc1.group1.tag1'
		);
	});

	it('focus 複数要素はカンマ区切りで、各要素は encodeURIComponent される', () => {
		expect(monitorHref({ groupId: 5, focus: ['plc1.group1.tag 1', 'plc1.group1.tag&2'] })).toBe(
			'/monitor?group=5&focus=plc1.group1.tag%201,plc1.group1.tag%262'
		);
	});

	it('focus が空配列・未指定なら focus パラメータを付けない', () => {
		expect(monitorHref({ groupId: 5, focus: [] })).toBe('/monitor?group=5');
		expect(monitorHref({ groupId: 5 })).toBe('/monitor?group=5');
	});

	it('focus のみ指定（group/connection 無し）', () => {
		expect(monitorHref({ focus: ['plc1.group1.tag1'] })).toBe('/monitor?focus=plc1.group1.tag1');
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

describe('resolveRegistrationTarget', () => {
	// T19 S1-c（UX-33）: `resolveGroupIdFromTreeSelection` と違い、virtual
	// （calc/mem）配下のグループも登録対象として扱う - 右クリック「グループ
	// 配下にタグを作成」（tagTreeContextMenu.ts）と同じ権限に揃えるため。
	const connections = [connection({ id: 1 }), CALC, MEM];
	const plcGroup = group({ id: 10, name: 'plc-group', plcConnectionId: 1 });
	const calcGroup = group({ id: 20, name: 'calc-group', plcConnectionId: CALC.id });
	const memGroup = group({ id: 30, name: 'mem-group', plcConnectionId: MEM.id });
	const groups = [plcGroup, calcGroup, memGroup];

	it('"all" 選択は null（登録操作を提示しない）', () => {
		expect(resolveRegistrationTarget({ type: 'all' }, groups, connections)).toBeNull();
	});

	it('接続選択は null（グループが一意に決まらない）', () => {
		expect(
			resolveRegistrationTarget({ type: 'connection', id: 1 }, groups, connections)
		).toBeNull();
	});

	it('存在しないグループ ID の選択は null', () => {
		expect(resolveRegistrationTarget({ type: 'group', id: 999 }, groups, connections)).toBeNull();
	});

	it('実グループ（plc）選択: tagKind=plc・連続登録も使える', () => {
		const target = resolveRegistrationTarget({ type: 'group', id: 10 }, groups, connections);
		expect(target).toEqual({
			groupId: 10,
			groupName: 'plc-group',
			tagKind: 'plc',
			supportsContinuous: true
		});
	});

	it('calc 配下のグループ選択: tagKind=computed・連続登録は使えない', () => {
		const target = resolveRegistrationTarget({ type: 'group', id: 20 }, groups, connections);
		expect(target).toEqual({
			groupId: 20,
			groupName: 'calc-group',
			tagKind: 'computed',
			supportsContinuous: false
		});
	});

	it('mem 配下のグループ選択: tagKind=internal・連続登録は使えない', () => {
		const target = resolveRegistrationTarget({ type: 'group', id: 30 }, groups, connections);
		expect(target).toEqual({
			groupId: 30,
			groupName: 'mem-group',
			tagKind: 'internal',
			supportsContinuous: false
		});
	});
});
