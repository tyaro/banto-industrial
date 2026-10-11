/**
 * 「Hub から追加」のダイアログの判断（`hubRegistration.ts`、#383 段階3 P4）。
 */
import { describe, expect, it } from 'vitest';
import type { HubCatalog, HubTag } from './hubAdmin';
import type { CollectionGroup, PlcConnection } from './tagRegistryAdmin';
import {
	applyCatalogLoad,
	applyLegacyDismissal,
	EMPTY_DIALOG_SELECTION,
	availabilityLabel,
	buildRegistration,
	defaultTarget,
	DEFAULT_HUB_GROUP_PERIOD_MS,
	dialogDirty,
	filterHubTags,
	hubGroupsOf,
	hubTagAvailability,
	initialSelection,
	isRegistrableDataType,
	legacyNoticeText,
	registrationBlocker,
	registrationSuccessMessage,
	rowErrorsByExternalName,
	targetErrorMessages
} from './hubRegistration';

function tag(externalName: string, dataType = 'f32', overrides: Partial<HubTag> = {}): HubTag {
	return {
		externalName,
		name: externalName.split('.').pop() ?? externalName,
		dataType,
		unit: '℃',
		decimals: 1,
		tagKind: 'plc',
		...overrides
	};
}

function catalog(overrides: Partial<HubCatalog> = {}): HubCatalog {
	return {
		status: { state: 'connected', tagCount: 4 },
		tags: [
			tag('L.G.temp'),
			tag('L.G.run', 'bit'),
			tag('L.G.label', 'string'),
			tag('L.G.done', 'i16')
		],
		registered: ['L.G.done'],
		legacySelection: [],
		...overrides
	};
}

describe('isRegistrableDataType / hubTagAvailability', () => {
	it('数値と bit だけが登録できる', () => {
		for (const t of ['bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'i64', 'u64', 'f64']) {
			expect(isRegistrableDataType(t)).toBe(true);
		}
		for (const t of ['string', 'struct', '', 'F32']) {
			expect(isRegistrableDataType(t)).toBe(false);
		}
	});

	it('登録済み・記録できない型は理由付きで選べない（登録済みを先に言う）', () => {
		const registered = new Set(['L.G.done', 'L.G.oldtext']);
		const table: [HubTag, ReturnType<typeof hubTagAvailability>][] = [
			[tag('L.G.temp'), { selectable: true }],
			[tag('L.G.done', 'i16'), { selectable: false, reason: 'registered' }],
			[tag('L.G.label', 'string'), { selectable: false, reason: 'unsupportedType' }],
			[tag('L.G.s', 'struct'), { selectable: false, reason: 'unsupportedType' }],
			[tag('L.G.oldtext', 'string'), { selectable: false, reason: 'registered' }]
		];
		for (const [row, expected] of table) {
			expect(hubTagAvailability(row, registered), row.externalName).toEqual(expected);
		}
		expect(availabilityLabel({ selectable: false, reason: 'registered' }, 'f32')).toBe('登録済み');
		expect(availabilityLabel({ selectable: false, reason: 'unsupportedType' }, 'struct')).toContain(
			'struct'
		);
		expect(availabilityLabel({ selectable: true }, 'f32')).toBe('');
	});
});

describe('filterHubTags', () => {
	const tags = [
		tag('Line1.Fast.Temp01', 'f32', { unit: '℃' }),
		tag('Line1.Fast.Run', 'bit', { unit: null }),
		tag('Line2.Slow.Flow', 'f64', { unit: 'L/min' })
	];

	it('空の検索は全件（catalog の並びのまま）', () => {
		expect(filterHubTags(tags, '  ').map((t) => t.externalName)).toEqual([
			'Line1.Fast.Temp01',
			'Line1.Fast.Run',
			'Line2.Slow.Flow'
		]);
	});

	it('外部名・単位・型の部分一致、大文字小文字を区別しない、語はすべて含む', () => {
		expect(filterHubTags(tags, 'line1').length).toBe(2);
		expect(filterHubTags(tags, 'L/MIN').map((t) => t.externalName)).toEqual(['Line2.Slow.Flow']);
		expect(filterHubTags(tags, 'line1 bit').map((t) => t.externalName)).toEqual(['Line1.Fast.Run']);
		expect(filterHubTags(tags, 'nothing')).toEqual([]);
	});
});

describe('initialSelection / legacyNoticeText（移行前の選択の事前チェック）', () => {
	it('移行前の選択が無ければ何もチェックせず、注記も出さない', () => {
		const initial = initialSelection(catalog(), false);
		expect(initial).toEqual({ selected: [], showLegacyNotice: false, legacySkipped: [] });
	});

	it('選べるものだけ事前にチェックし、消えた・登録済み・記録できない型は添える', () => {
		const initial = initialSelection(
			catalog({ legacySelection: ['L.G.temp', 'L.G.done', 'L.G.label', 'L.G.gone'] }),
			false
		);
		expect(initial.selected).toEqual(['L.G.temp']);
		expect(initial.showLegacyNotice).toBe(true);
		expect(initial.legacySkipped).toEqual(['L.G.done', 'L.G.label', 'L.G.gone']);
		const text = legacyNoticeText(initial);
		expect(text).toContain('以前の Hub 接続の画面で選んでいた');
		expect(text).toContain('L.G.gone');
	});

	it('1 度扱った（破棄した・登録した）後は事前チェックしない', () => {
		const initial = initialSelection(catalog({ legacySelection: ['L.G.temp'] }), true);
		expect(initial).toEqual({ selected: [], showLegacyNotice: false, legacySkipped: [] });
	});

	it('catalog を読めていない間は注記だけ出し、何もチェックしない', () => {
		const initial = initialSelection(catalog({ tags: null, legacySelection: ['L.G.temp'] }), false);
		expect(initial).toEqual({ selected: [], showLegacyNotice: true, legacySkipped: [] });
		expect(legacyNoticeText(initial)).not.toContain('チェックしていません');
	});
});

describe('hubGroupsOf / defaultTarget / registrationBlocker', () => {
	const connections = [
		{ id: 1, protocol: 'slmp' },
		{ id: 2, protocol: 'hub' }
	] as PlcConnection[];
	const groups = [
		{ id: 10, plcConnectionId: 1, name: 'plc-g' },
		{ id: 20, plcConnectionId: 2, name: 'hub-g' }
	] as CollectionGroup[];

	it('Hub 接続の下のグループだけが候補', () => {
		expect(hubGroupsOf(connections, groups).map((g) => g.id)).toEqual([20]);
		expect(hubGroupsOf(connections.slice(0, 1), groups)).toEqual([]);
	});

	it('Hub のグループがあれば既存、無ければ新しいグループ（周期は既定 1000 ms）', () => {
		expect(defaultTarget(hubGroupsOf(connections, groups))).toEqual({
			kind: 'existing',
			collectionGroupId: 20
		});
		expect(defaultTarget([])).toEqual({
			kind: 'new',
			name: '',
			periodMs: DEFAULT_HUB_GROUP_PERIOD_MS
		});
		expect(DEFAULT_HUB_GROUP_PERIOD_MS).toBe(1000);
	});

	it('選択 0 件・登録先未指定・新しいグループの名前が空なら止める', () => {
		const table: [number, Parameters<typeof registrationBlocker>[1], boolean][] = [
			[0, { kind: 'existing', collectionGroupId: 20 }, true],
			[1, { kind: 'existing', collectionGroupId: null }, true],
			[1, { kind: 'new', name: '  ', periodMs: 1000 }, true],
			[1, { kind: 'new', name: 'g', periodMs: 1000 }, false],
			[2, { kind: 'existing', collectionGroupId: 20 }, false]
		];
		for (const [count, target, blocked] of table) {
			expect(registrationBlocker(count, target) !== null, JSON.stringify([count, target])).toBe(
				blocked
			);
		}
	});
});

describe('buildRegistration', () => {
	const tags = catalog().tags ?? [];
	const registered = new Set(['L.G.done']);

	it('catalog の並びで、選んだ・選べる行だけを複製して送る', () => {
		const registration = buildRegistration(
			tags,
			new Set(['L.G.run', 'L.G.temp', 'L.G.label', 'L.G.done']),
			registered,
			{ kind: 'existing', collectionGroupId: 20 }
		);
		expect(registration.collectionGroupId).toBe(20);
		expect(registration.newGroup).toBeNull();
		expect(registration.tags).toEqual([
			{ externalName: 'L.G.temp', name: 'temp', dataType: 'f32', unit: '℃', decimals: 1 },
			{ externalName: 'L.G.run', name: 'run', dataType: 'bit', unit: '℃', decimals: 1 }
		]);
	});

	it('新しいグループは名前の前後の空白を落として送る', () => {
		const registration = buildRegistration(tags, new Set(['L.G.temp']), registered, {
			kind: 'new',
			name: ' hub-fast ',
			periodMs: 500
		});
		expect(registration.collectionGroupId).toBeNull();
		expect(registration.newGroup).toEqual({ name: 'hub-fast', periodMs: 500 });
	});
});

describe('結果の表示', () => {
	it('行のエラーは外部名ごとにまとめる（黙って改名しない - その行に出す）', () => {
		const map = rowErrorsByExternalName({
			rowErrors: [
				{
					index: 0,
					externalName: 'L2.G.temp',
					fieldErrors: [{ field: 'name', message: 'この収集グループ内では既に使用されています' }]
				},
				{
					index: 1,
					externalName: 'L.G.done',
					fieldErrors: [{ field: 'externalName', message: '既に登録されています' }]
				}
			]
		});
		expect(map.get('L2.G.temp')).toEqual(['この収集グループ内では既に使用されています']);
		expect(map.get('L.G.done')).toEqual(['既に登録されています']);
		expect(targetErrorMessages({ fieldErrors: [{ field: 'newGroupName', message: 'x' }] })).toEqual(
			['x']
		);
	});

	it('成功の文言は件数と、暗黙に作ったもの、収集の再起動を言う', () => {
		const text = registrationSuccessMessage({
			tags: [{}, {}] as never,
			groupCreated: true,
			connectionCreated: true
		});
		expect(text).toContain('2 件');
		expect(text).toContain('Hub 接続を作成しました');
		expect(text).toContain('収集グループを作成しました');
		expect(text).toContain('収集を再起動');
	});
});

describe('dialogDirty', () => {
	it('事前チェックのままなら未保存ではなく、変えたら・名前を入れたら未保存', () => {
		const existing = { kind: 'existing', collectionGroupId: 20 } as const;
		expect(dialogDirty(new Set(['a']), ['a'], existing)).toBe(false);
		expect(dialogDirty(new Set(), [], existing)).toBe(false);
		expect(dialogDirty(new Set(['a', 'b']), ['a'], existing)).toBe(true);
		expect(dialogDirty(new Set(['b']), ['a'], existing)).toBe(true);
		expect(dialogDirty(new Set(), [], { kind: 'new', name: 'g', periodMs: 1000 })).toBe(true);
		expect(dialogDirty(new Set(), [], { kind: 'new', name: ' ', periodMs: 1000 })).toBe(false);
	});
});

describe('applyCatalogLoad（読み込みのたびの選択。オーナーレビュー P2）', () => {
	it('初めて読めたら事前チェックを入れ、読めた印を立てる', () => {
		const state = applyCatalogLoad(
			EMPTY_DIALOG_SELECTION,
			catalog({ legacySelection: ['L.G.temp'] })
		);
		expect(state.selected).toEqual(['L.G.temp']);
		expect(state.initial.showLegacyNotice).toBe(true);
		expect(state.tagsRead).toBe(true);
	});

	it('一時的に読めない（tags: null）ときは選択も事前チェックの値も変えず、次に読めたら照合する', () => {
		const first = applyCatalogLoad(
			EMPTY_DIALOG_SELECTION,
			catalog({ legacySelection: ['L.G.temp'] })
		);
		const picked = { ...first, selected: [...first.selected, 'L.G.run'] };
		const outage = applyCatalogLoad(picked, catalog({ tags: null }));
		expect(outage).toBe(picked);
		const back = applyCatalogLoad(outage, catalog());
		expect(back.selected).toEqual(['L.G.temp', 'L.G.run']);
		expect(back.initial).toBe(first.initial);
	});

	it('2 回目以降に読めたら、消えた・登録済みになったものだけ外す', () => {
		const first = applyCatalogLoad(EMPTY_DIALOG_SELECTION, catalog());
		const picked = { ...first, selected: ['L.G.temp', 'L.G.run'] };
		const next = applyCatalogLoad(picked, catalog({ tags: [tag('L.G.temp')], registered: [] }));
		expect(next.selected).toEqual(['L.G.temp']);
		const nowRegistered = applyCatalogLoad(picked, catalog({ registered: ['L.G.temp'] }));
		expect(nowRegistered.selected).toEqual(['L.G.run']);
	});

	it('開いた直後に読めなかったら注記だけ出し、読めた時点で事前チェックする', () => {
		const outage = applyCatalogLoad(
			EMPTY_DIALOG_SELECTION,
			catalog({ tags: null, legacySelection: ['L.G.temp'] })
		);
		expect(outage.selected).toEqual([]);
		expect(outage.tagsRead).toBe(false);
		expect(outage.initial.showLegacyNotice).toBe(true);
		const back = applyCatalogLoad(outage, catalog({ legacySelection: ['L.G.temp'] }));
		expect(back.selected).toEqual(['L.G.temp']);
		expect(back.tagsRead).toBe(true);
	});
});

describe('applyLegacyDismissal（「以前の選択を使わない」と遅れて届く読み込み。オーナーレビュー P2）', () => {
	it('事前チェックしたものだけ外し、自分で選んだものは残す', () => {
		const first = applyCatalogLoad(
			EMPTY_DIALOG_SELECTION,
			catalog({ legacySelection: ['L.G.temp'] })
		);
		const picked = { ...first, selected: [...first.selected, 'L.G.run'] };
		const dismissed = applyLegacyDismissal(picked);
		expect(dismissed.selected).toEqual(['L.G.run']);
		expect(dismissed.initial.showLegacyNotice).toBe(false);
		expect(dismissed.legacyDismissed).toBe(true);
	});

	it('初回が読めず（tags: null）、破棄の後に破棄より前の応答が遅れて届いても、以前の選択と注記は戻らない', () => {
		// 1. 開いた直後: タグ一覧は読めず、以前の選択 [temp] の注記だけ。
		const outage = applyCatalogLoad(
			EMPTY_DIALOG_SELECTION,
			catalog({ tags: null, legacySelection: ['L.G.temp'] })
		);
		expect(outage.initial.showLegacyNotice).toBe(true);
		// 2. 再試行の GET が飛んでいる間に「以前の選択を使わない」が成功する。
		const dismissed = applyLegacyDismissal(outage);
		// 3. 破棄より前にサーバーで作られた応答（以前の選択 [temp] を含む、初めて読めた一覧）が届く。
		const late = applyCatalogLoad(dismissed, catalog({ legacySelection: ['L.G.temp'] }));
		expect(late.selected).toEqual([]);
		expect(late.initial.showLegacyNotice).toBe(false);
		expect(late.tagsRead).toBe(true);
		expect(late.legacyDismissed).toBe(true);
	});

	it('読めた後の破棄でも、後から届く読み込みが以前の選択を事前チェックし直さない', () => {
		const first = applyCatalogLoad(
			EMPTY_DIALOG_SELECTION,
			catalog({ legacySelection: ['L.G.temp'] })
		);
		const dismissed = applyLegacyDismissal(first);
		const late = applyCatalogLoad(dismissed, catalog({ legacySelection: ['L.G.temp'] }));
		expect(late.selected).toEqual([]);
		expect(late.initial.showLegacyNotice).toBe(false);
	});
});
