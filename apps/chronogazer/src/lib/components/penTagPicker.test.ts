import { describe, expect, it } from 'vitest';
import type { HubTag } from '#lib/banto/hubAdmin.js';
import type { CollectionGroup, PlcConnection, Tag } from '#lib/banto/tagRegistryAdmin.js';
import {
	HUB_ROOT_ID,
	MAX_RESULT_ROWS,
	ORPHAN_NODE_ID,
	addBlocker,
	addableSelection,
	buildPickerCatalog,
	describePenTag,
	hubNoteOf,
	limitRows,
	penFromRow,
	rowMark,
	rowState,
	searchRows,
	type PickerCatalogInput
} from './penTagPicker';

function connection(id: number, name: string, protocol: string): PlcConnection {
	return {
		id,
		name,
		protocol,
		enabled: true,
		settings: {},
		simulation: false
	} as PlcConnection;
}

function group(id: number, name: string, plcConnectionId: number): CollectionGroup {
	return { id, name, plcConnectionId, periodMs: 1000, enabled: true } as CollectionGroup;
}

function tag(id: number, name: string, collectionGroupId: number, address = `D${id}`): Tag {
	return {
		id,
		name,
		collectionGroupId,
		address,
		dataType: 'f32',
		unit: 'degC',
		decimals: 1,
		enabled: true
	} as unknown as Tag;
}

function hubTag(
	externalName: string,
	connectionName: string,
	groupName: string,
	dataType = 'f32'
): HubTag {
	return {
		externalName,
		connection: connectionName,
		group: groupName,
		name: externalName.split('/').pop() as string,
		dataType,
		unit: null,
		decimals: 0,
		tagKind: 'plc'
	};
}

const connections = [connection(1, 'PLC-A', 'slmp'), connection(2, 'Hub', 'hub')];
const groups = [group(10, 'G1', 1), group(20, 'HG', 2)];

function input(overrides: Partial<PickerCatalogInput> = {}): PickerCatalogInput {
	return {
		connections,
		collectionGroups: groups,
		tags: [
			tag(1, 'Temp', 10),
			tag(2, 'Flow', 10),
			tag(3, 'Reg', 20, 'H/G/T1'),
			tag(4, 'Old', 20, 'H/G/GONE')
		],
		hub: {
			status: { state: 'connected', tagCount: 4 },
			tags: [
				hubTag('H/G/T1', 'H', 'G'),
				hubTag('H/G/T2', 'H', 'G'),
				hubTag('H/G/S1', 'H', 'G', 'string'),
				hubTag('H/X/Y1', 'H', 'X')
			],
			registered: ['H/G/T1', 'H/G/GONE']
		},
		hubNote: null,
		...overrides
	};
}

describe('buildPickerCatalog', () => {
	it('Hub 以外の接続先と Hub の根を作り、Hub 接続は (a) に出さない', () => {
		const catalog = buildPickerCatalog(input());
		expect(catalog.nodes.map((n) => n.id)).toEqual(['rec:conn:1', HUB_ROOT_ID]);
		const rec = catalog.nodes[0];
		expect(rec.children?.map((c) => c.id)).toEqual(['rec:group:10']);
		expect(rec.children?.[0].label).toBe('G1（2）');
		const hub = catalog.nodes[1];
		expect(hub.label).toBe('Hub');
		// 接続先 H → 収集グループ G・X、カタログに無い記録項目。
		expect(hub.children?.map((c) => c.id)).toEqual(['hub:conn:H', ORPHAN_NODE_ID]);
		expect(hub.children?.[0].children?.map((c) => c.label)).toEqual(['G（3）', 'X（1）']);
	});

	it('(a) の行は tagId で追加できる記録項目（名前順）', () => {
		const rows = buildPickerCatalog(input()).rowsByNode.get('rec:group:10') ?? [];
		expect(rows.map((r) => r.name)).toEqual(['Flow', 'Temp']);
		expect(rows[0].pick).toEqual({ kind: 'tag', tagId: 2 });
		expect(rows[0].fromHub).toBe(false);
	});

	it('(b) 登録済みの Hub タグは tagId、未登録は hubTag、登録できない型は理由つきで選べない', () => {
		const catalog = buildPickerCatalog(input());
		const rows = catalog.rowsByNode.get('hub:group:H\u0000G') ?? [];
		const byName = new Map(rows.map((r) => [r.address, r]));
		expect(byName.get('H/G/T1')).toMatchObject({
			kind: 'recorded',
			key: 'tag:3',
			pick: { kind: 'tag', tagId: 3 }
		});
		const unregistered = byName.get('H/G/T2');
		expect(unregistered).toMatchObject({ kind: 'unregistered', key: 'hub:H/G/T2' });
		expect(unregistered?.pick).toMatchObject({
			kind: 'hub',
			hubTag: { externalName: 'H/G/T2', dataType: 'f32', decimals: 0 },
			connection: 'H',
			group: 'G'
		});
		const blocked = byName.get('H/G/S1');
		expect(blocked).toMatchObject({ kind: 'blocked', pick: null });
		expect(blocked?.blockedReason).toContain('string 型は記録できません');
		expect(catalog.registeredByName.get('H/G/T1')).toBe(3);
	});

	it('カタログに無い Hub 接続の記録項目は専用のグループに出る', () => {
		const rows = buildPickerCatalog(input()).rowsByNode.get(ORPHAN_NODE_ID) ?? [];
		expect(rows.map((r) => [r.name, r.key])).toEqual([['Old', 'tag:4']]);
	});

	it('カタログが読めないとき、Hub の記録項目は全部カタログに無い扱いで選べる。根に状態を出す', () => {
		const catalog = buildPickerCatalog(
			input({ hub: null, hubNote: 'カタログを読めませんでした（Hubに到達できません）' })
		);
		const hub = catalog.nodes.find((n) => n.id === HUB_ROOT_ID);
		expect(hub?.label).toContain('Hubに到達できません');
		expect(catalog.rowsByNode.get(ORPHAN_NODE_ID)?.map((r) => r.name)).toEqual(['Old', 'Reg']);
		// (a) は普通に使える。
		expect(catalog.rowsByNode.get('rec:group:10')).toHaveLength(2);
	});

	it('Hub 接続が無いときも Hub の根は出る', () => {
		const catalog = buildPickerCatalog(
			input({ connections: [connections[0]], collectionGroups: [groups[0]], tags: [], hub: null })
		);
		expect(catalog.nodes.map((n) => n.id)).toEqual(['rec:conn:1', HUB_ROOT_ID]);
		expect(catalog.nodes[1].children).toEqual([]);
	});

	it('Hub 側で登録済みなのに記録項目の一覧に無い外部名は、選べず更新を促す', () => {
		const rows = buildPickerCatalog(input({ tags: [] })).rowsByNode.get('hub:group:H\u0000G') ?? [];
		const row = rows.find((r) => r.address === 'H/G/T1');
		expect(row?.kind).toBe('blocked');
		expect(row?.blockedReason).toContain('一覧を更新');
	});
});

describe('hubNoteOf', () => {
	const label = () => 'ラベル';
	it('読み込み・誤り・読めない・0 件・通常', () => {
		expect(hubNoteOf(null, true, null, label)).toBe('読み込み中…');
		expect(hubNoteOf(null, false, '失敗', label)).toBe('カタログを読めませんでした（失敗）');
		expect(
			hubNoteOf(
				{ status: { state: 'unreachable', cause: 'transport' }, tags: null, registered: [] },
				false,
				null,
				label
			)
		).toBe('カタログを読めませんでした（ラベル）');
		expect(
			hubNoteOf(
				{ status: { state: 'connected', tagCount: 0 }, tags: [], registered: [] },
				false,
				null,
				label
			)
		).toBe('ラベル');
		expect(
			hubNoteOf(
				{
					status: { state: 'connected', tagCount: 1 },
					tags: [hubTag('a/b/c', 'a', 'b')],
					registered: []
				},
				false,
				null,
				label
			)
		).toBeNull();
		expect(hubNoteOf(null, false, null, label)).toBeNull();
	});
});

describe('行の状態・印・追加', () => {
	const catalog = buildPickerCatalog(input());
	const find = (key: string) => catalog.allRows.find((r) => r.key === key)!;

	it('追加済み（使用中のキー）・登録できない・追加できる', () => {
		const used = new Set(['tag:1', 'hub:H/G/T2']);
		expect(rowState(find('tag:1'), used)).toEqual({ kind: 'added' });
		expect(rowState(find('hub:H/G/T2'), used)).toEqual({ kind: 'added' });
		expect(rowState(find('tag:2'), used)).toEqual({ kind: 'addable' });
		expect(rowState(find('hub:H/G/S1'), used).kind).toBe('blocked');
	});

	it('印: 追加済み・未登録・記録中・登録できない理由', () => {
		const none = new Set<string>();
		expect(rowMark(find('tag:1'), rowState(find('tag:1'), new Set(['tag:1'])))).toBe('追加済み');
		expect(rowMark(find('hub:H/G/T2'), rowState(find('hub:H/G/T2'), none))).toBe(
			'未登録・保存で記録に追加'
		);
		expect(rowMark(find('tag:3'), rowState(find('tag:3'), none))).toBe('記録中');
		expect(rowMark(find('tag:1'), rowState(find('tag:1'), none))).toBe('');
		expect(rowMark(find('hub:H/G/S1'), rowState(find('hub:H/G/S1'), none))).toContain(
			'記録できません'
		);
	});

	it('選択のうち追加できるものだけを、行の並びで返す', () => {
		const rows = catalog.rowsByNode.get('hub:group:H\u0000G') ?? [];
		const selected = new Set(['hub:H/G/T2', 'hub:H/G/S1', 'tag:3']);
		const result = addableSelection(rows, selected, new Set(['tag:3']));
		expect(result.map((r) => r.key)).toEqual(['hub:H/G/T2']);
	});

	it('penFromRow: tagId と hubTag（出どころつき）', () => {
		expect(penFromRow(find('tag:1'))).toEqual({
			tagId: 1,
			hubTag: null,
			hubSource: null,
			colorSlot: null
		});
		const pen = penFromRow(find('hub:H/G/T2'));
		expect(pen.tagId).toBeNull();
		expect(pen.hubTag?.externalName).toBe('H/G/T2');
		expect(pen.hubSource).toEqual({ connection: 'H', group: 'G' });
		expect(() => penFromRow(find('hub:H/G/S1'))).toThrow();
	});
});

describe('検索と打ち切り', () => {
	const catalog = buildPickerCatalog(input());

	it('名前・外部名・接続先・収集グループ・単位・型の部分一致（空白区切りは AND、大文字小文字を区別しない）', () => {
		expect(searchRows(catalog.allRows, '')).toEqual([]);
		expect(searchRows(catalog.allRows, 'temp').map((r) => r.name)).toEqual(['Temp']);
		expect(
			searchRows(catalog.allRows, 'plc-a g1')
				.map((r) => r.name)
				.sort()
		).toEqual(['Flow', 'Temp']);
		expect(searchRows(catalog.allRows, 'H/G/T').length).toBe(2);
		expect(searchRows(catalog.allRows, 'degc flow').map((r) => r.name)).toEqual(['Flow']);
		expect(searchRows(catalog.allRows, 'nomatch')).toEqual([]);
	});

	it('200 件で打ち切り、超えた分の件数を返す', () => {
		const many = Array.from({ length: 250 }, (_, i) => i);
		const limited = limitRows(many);
		expect(limited.shown).toHaveLength(MAX_RESULT_ROWS);
		expect(limited.hidden).toBe(50);
		expect(limitRows(many.slice(0, 200))).toEqual({ shown: many.slice(0, 200), hidden: 0 });
	});
});

describe('ペンの上限', () => {
	it('満杯・残りを超える選択は止め、収まれば null', () => {
		expect(addBlocker(1, 0, 8)).toContain('8 本まで');
		expect(addBlocker(3, 2, 8)).toContain('あと 2 本');
		expect(addBlocker(2, 2, 8)).toBeNull();
		expect(addBlocker(1, 5, 8)).toBeNull();
	});
});

describe('describePenTag', () => {
	const tagsById = new Map([[1, tag(1, 'Temp', 10)]]);
	it('記録項目・未登録の Hub タグ・見つからない・未選択', () => {
		expect(describePenTag({ tagId: 1, hubTag: null }, tagsById, groups, connections)).toEqual({
			name: 'Temp',
			connection: 'PLC-A',
			group: 'G1',
			unregistered: false
		});
		expect(
			describePenTag(
				{
					tagId: null,
					hubTag: { externalName: 'H/G/T2', name: 'T2', dataType: 'f32', unit: null, decimals: 0 },
					hubSource: { connection: 'H', group: 'G' }
				},
				tagsById,
				groups,
				connections
			)
		).toEqual({ name: 'T2', connection: 'H', group: 'G', unregistered: true });
		expect(
			describePenTag({ tagId: 99, hubTag: null }, tagsById, groups, connections)?.name
		).toContain('見つからない');
		expect(describePenTag({ tagId: null, hubTag: null }, tagsById, groups, connections)).toBeNull();
	});
});
