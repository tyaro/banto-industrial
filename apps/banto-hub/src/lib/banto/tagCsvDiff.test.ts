/**
 * T18-3d CSV 新規/更新分離: `tagCsvDiff.ts::classifyCsvUpdate` のユニット
 * テスト。`tagCsv.ts::parseTagsCsv` を通した実物の `ParsedCsvTagRow[]` を
 * 入力に使い、「既存タグと突き合わせて added/changed/unchanged/error に
 * 正しく分類されるか」「`updateRows` には changed のみが積まれるか」
 * 「`null`(既存)/`undefined`(CSV 未入力)の差だけでは changed 扱いになら
 * ないか」を固定する。
 */
import { describe, expect, it } from 'vitest';
import { classifyCsvUpdate } from './tagCsvDiff';
import { parseTagsCsv, TAG_CSV_COLUMNS, type ParsedCsvTagRow } from './tagCsv';
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';

// --- 共通フィクスチャ ---------------------------------------------------------

const CONN_A: PlcConnection = {
	id: 1,
	name: 'ConnA',
	protocol: 'modbus-tcp',
	host: '127.0.0.1',
	port: 502,
	unitId: 1,
	enabled: true,
	simulation: false,
	wordOrder: 'low_high'
};
const CONNECTIONS: PlcConnection[] = [CONN_A];

const GROUP_A: CollectionGroup = {
	id: 10,
	name: 'GroupA',
	plcConnectionId: CONN_A.id,
	periodMs: 1000,
	enabled: true
};
const GROUP_B: CollectionGroup = {
	id: 11,
	name: 'GroupB',
	plcConnectionId: CONN_A.id,
	periodMs: 1000,
	enabled: true
};
const GROUPS: CollectionGroup[] = [GROUP_A, GROUP_B];

function makeTag(overrides: Partial<Tag> = {}): Tag {
	return {
		id: 1,
		name: 'Tag1',
		collectionGroupId: GROUP_A.id,
		address: 'D100',
		dataType: 'i16',
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

type CsvColumn = (typeof TAG_CSV_COLUMNS)[number];
type CsvRowFields = Record<CsvColumn, string>;

/** 妥当な最小 plc タグ1行分の既定値。`makeTag()` の既定値と対応する。 */
const DEFAULT_ROW: CsvRowFields = {
	connection: 'ConnA',
	group: 'GroupA',
	name: 'Tag1',
	address: 'D100',
	dataType: 'i16',
	stringLength: '',
	unit: '',
	decimals: '',
	rawLo: '',
	rawHi: '',
	engLo: '',
	engHi: '',
	thresholdH: '',
	thresholdHh: '',
	thresholdL: '',
	thresholdLl: '',
	enabled: '',
	writable: '',
	tagKind: '',
	expression: '',
	retain: ''
};

function csvRow(overrides: Partial<CsvRowFields> = {}): CsvRowFields {
	return { ...DEFAULT_ROW, ...overrides };
}

/** ヘッダ + 複数データ行から CSV 全文を組み立てる。 */
function buildCsv(rows: CsvRowFields[]): string {
	const lines = [
		[...TAG_CSV_COLUMNS].join(','),
		...rows.map((r) => TAG_CSV_COLUMNS.map((c) => r[c]).join(','))
	];
	return lines.join('\r\n') + '\r\n';
}

/** CSV 行群を実際に `parseTagsCsv` に通して `ParsedCsvTagRow[]` を得る。 */
function parsed(rows: CsvRowFields[]): ParsedCsvTagRow[] {
	const result = parseTagsCsv(buildCsv(rows), CONNECTIONS, GROUPS);
	if (!result.ok) {
		throw new Error(
			`テストフィクスチャの CSV がパースエラーになった: ${JSON.stringify(result.errors)}`
		);
	}
	return result.rows;
}

// ==============================================================================
// added
// ==============================================================================

describe('classifyCsvUpdate: added', () => {
	it('既存に一致するキー(collectionGroupId,name)が無ければ added になる', () => {
		const rows = parsed([csvRow({ name: 'NewTag' })]);
		const result = classifyCsvUpdate(rows, [makeTag({ name: 'Tag1' })]);
		expect(result.rows).toHaveLength(1);
		expect(result.rows[0].category).toBe('added');
		expect(result.rows[0].message).toContain('既存更新モード');
		expect(result.addedCount).toBe(1);
	});

	it('added 行は tagId/expectedRevision/diffs を持たない', () => {
		const rows = parsed([csvRow({ name: 'NewTag' })]);
		const result = classifyCsvUpdate(rows, []);
		expect(result.rows[0].tagId).toBeUndefined();
		expect(result.rows[0].expectedRevision).toBeUndefined();
		expect(result.rows[0].diffs).toBeUndefined();
	});

	it('added 行は updateRows に含まれない', () => {
		const rows = parsed([csvRow({ name: 'NewTag' })]);
		const result = classifyCsvUpdate(rows, []);
		expect(result.updateRows).toHaveLength(0);
	});

	it('同名でも collectionGroupId が違えば別タグ扱いで added になる', () => {
		// 既存タグは GroupA 配下の Tag1。CSV 行は同名 Tag1 だが GroupB 配下を
		// 指定しているので、突き合わせキーが一致せず added になるはず。
		const existing = makeTag({ name: 'Tag1', collectionGroupId: GROUP_A.id });
		const rows = parsed([csvRow({ name: 'Tag1', group: 'GroupB' })]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.rows[0].category).toBe('added');
	});
});

// ==============================================================================
// changed
// ==============================================================================

describe('classifyCsvUpdate: changed', () => {
	it('既存タグと値が異なれば changed になり、diffs に変更フィールドが入る', () => {
		const existing = makeTag({ address: 'D100' });
		const rows = parsed([csvRow({ address: 'D200' })]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.rows[0].category).toBe('changed');
		expect(result.changedCount).toBe(1);
		expect(result.rows[0].diffs).toEqual([{ field: 'address', from: 'D100', to: 'D200' }]);
	});

	it('changed 行は tagId/expectedRevision に既存タグの id/revision を持つ', () => {
		const existing = makeTag({ id: 42, revision: 7, address: 'D100' });
		const rows = parsed([csvRow({ address: 'D200' })]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.rows[0].tagId).toBe(42);
		expect(result.rows[0].expectedRevision).toBe(7);
	});

	it('changed 行のみ updateRows に積まれ、id/expectedRevision と CSV の新値が入る', () => {
		const existing = makeTag({ id: 42, revision: 7, address: 'D100', decimals: 0 });
		const rows = parsed([csvRow({ address: 'D200', decimals: '2' })]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.updateRows).toHaveLength(1);
		expect(result.updateRows[0]).toMatchObject({
			id: 42,
			expectedRevision: 7,
			address: 'D200',
			decimals: 2,
			name: 'Tag1',
			collectionGroupId: GROUP_A.id
		});
	});

	it('複数フィールドが変われば diffs に複数エントリが入る', () => {
		const existing = makeTag({ address: 'D100', enabled: true, unit: 'kPa' });
		const rows = parsed([csvRow({ address: 'D200', enabled: 'false', unit: 'MPa' })]);
		const result = classifyCsvUpdate(rows, [existing]);
		const fields = result.rows[0].diffs?.map((d) => d.field).sort();
		expect(fields).toEqual(['address', 'enabled', 'unit']);
	});

	it('boolean の差分は「オン」/「オフ」の表示文字列になる(diffFormRecords 再利用)', () => {
		const existing = makeTag({ enabled: true });
		const rows = parsed([csvRow({ enabled: 'false' })]);
		const result = classifyCsvUpdate(rows, [existing]);
		const enabledDiff = result.rows[0].diffs?.find((d) => d.field === 'enabled');
		expect(enabledDiff).toEqual({ field: 'enabled', from: 'オン', to: 'オフ' });
	});
});

// ==============================================================================
// unchanged
// ==============================================================================

describe('classifyCsvUpdate: unchanged', () => {
	it('既存タグと全フィールド一致すれば unchanged になる', () => {
		const existing = makeTag();
		const rows = parsed([csvRow()]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.rows[0].category).toBe('unchanged');
		expect(result.unchangedCount).toBe(1);
	});

	it('unchanged 行は tagId/expectedRevision を持つが diffs は持たない', () => {
		const existing = makeTag({ id: 5, revision: 3 });
		const rows = parsed([csvRow()]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.rows[0].tagId).toBe(5);
		expect(result.rows[0].expectedRevision).toBe(3);
		expect(result.rows[0].diffs).toBeUndefined();
	});

	it('unchanged 行は updateRows に含まれない(無変更は無送信)', () => {
		const existing = makeTag();
		const rows = parsed([csvRow()]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.updateRows).toHaveLength(0);
	});

	it('既存の null(未設定)と CSV の空欄(undefined)は同じ「未設定」として unchanged 扱いになる', () => {
		// unit/rawLo/rawHi/engLo/engHi/threshold*/stringLength は既存タグでは
		// null、CSV で空欄なら parseTagsCsv 側は undefined を返す
		// (tagCsv.ts の numField/col 実装)。この差だけで changed にならない
		// ことを固定する(toComparableRecord の正規化)。
		const existing = makeTag({
			unit: null,
			rawLo: null,
			rawHi: null,
			engLo: null,
			engHi: null,
			thresholdH: null,
			thresholdHh: null,
			thresholdL: null,
			thresholdLl: null,
			stringLength: null,
			expression: null
		});
		const rows = parsed([csvRow()]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.rows[0].category).toBe('unchanged');
	});
});

// ==============================================================================
// error (CSV 内重複キー)
// ==============================================================================

describe('classifyCsvUpdate: error(CSV 内重複キー)', () => {
	it('同一キー(collectionGroupId,name)が CSV 内に複数あれば、両方とも error になる', () => {
		const rows = parsed([
			csvRow({ name: 'DupTag', address: 'D100' }),
			csvRow({ name: 'DupTag', address: 'D200' })
		]);
		const result = classifyCsvUpdate(rows, []);
		expect(result.rows.map((r) => r.category)).toEqual(['error', 'error']);
		expect(result.errorCount).toBe(2);
		expect(result.rows[0].message).toContain('重複');
	});

	it('error 行は updateRows に含まれない', () => {
		const rows = parsed([csvRow({ name: 'DupTag' }), csvRow({ name: 'DupTag' })]);
		const existing = makeTag({ name: 'DupTag' });
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.updateRows).toHaveLength(0);
	});

	it('重複キーが既存タグに一致していても error が優先される(changed/unchanged にならない)', () => {
		const existing = makeTag({ name: 'DupTag' });
		const rows = parsed([csvRow({ name: 'DupTag' }), csvRow({ name: 'DupTag', address: 'D999' })]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.rows.every((r) => r.category === 'error')).toBe(true);
	});

	it('同名でも collectionGroupId が異なれば重複とみなさない', () => {
		const rows = parsed([
			csvRow({ name: 'SameName', group: 'GroupA' }),
			csvRow({ name: 'SameName', group: 'GroupB' })
		]);
		const result = classifyCsvUpdate(rows, []);
		expect(result.rows.map((r) => r.category)).toEqual(['added', 'added']);
	});
});

// ==============================================================================
// lineNumber の保持・複数行混在
// ==============================================================================

describe('classifyCsvUpdate: lineNumber の保持・複数行混在', () => {
	it('rows の順序と lineNumber は parsed の順序をそのまま保つ', () => {
		const rows = parsed([
			csvRow({ name: 'Tag1' }), // line 2
			csvRow({ name: 'Tag2' }), // line 3
			csvRow({ name: 'Tag3' }) // line 4
		]);
		const result = classifyCsvUpdate(rows, []);
		expect(result.rows.map((r) => r.lineNumber)).toEqual([2, 3, 4]);
		expect(result.rows.map((r) => r.name)).toEqual(['Tag1', 'Tag2', 'Tag3']);
	});

	it('added/changed/unchanged/error が混在するケースで各カウントが一致する', () => {
		const existingChanged = makeTag({ name: 'ChangedTag', address: 'D100' });
		const existingUnchanged = makeTag({ name: 'UnchangedTag', address: 'D300' });
		const rows = parsed([
			csvRow({ name: 'AddedTag' }), // added
			csvRow({ name: 'ChangedTag', address: 'D200' }), // changed
			csvRow({ name: 'UnchangedTag', address: 'D300' }), // unchanged
			csvRow({ name: 'DupTag' }), // error(重複)
			csvRow({ name: 'DupTag' }) // error(重複)
		]);
		const result = classifyCsvUpdate(rows, [existingChanged, existingUnchanged]);
		expect(result.addedCount).toBe(1);
		expect(result.changedCount).toBe(1);
		expect(result.unchangedCount).toBe(1);
		expect(result.errorCount).toBe(2);
		expect(result.rows).toHaveLength(5);
		expect(result.updateRows).toHaveLength(1);
		expect(result.updateRows[0].name).toBe('ChangedTag');
	});
});
