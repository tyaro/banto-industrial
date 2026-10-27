/**
 * T18-3d CSV 新規/更新分離: `tagCsvDiff.ts::classifyCsvUpdate` のユニット
 * テスト。`tagCsv.ts::parseTagsCsv` を通した実物の `ParsedCsvTagRow[]` を
 * 入力に使い、「既存タグと突き合わせて added/changed/unchanged/error に
 * 正しく分類されるか」「`updateRows` には changed のみが積まれるか」
 * 「`null`(既存)/`undefined`(CSV 未入力)の差だけでは changed 扱いになら
 * ないか」を固定する。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';

// `tagCsvDiff.ts` は解析の API の HTTP ヘルパー（`tagRegistryAdmin.ts::parseTagAddresses`）を
// 使うので、`tagRegistryAdmin.test.ts` と同じ理由で `$state` を使うモジュールだけを
// 軽量なフェイクに差し替える。**解析の API 自体は差し替えない**（本物のヘルパーを通し、
// `fetch` をネットワークの境界でモックする、設計 §9「画面」）。
vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => 'test-token' }),
	ProviderError: class ProviderError extends Error {
		body: unknown;
		constructor(body: unknown) {
			super(
				typeof body === 'object' && body !== null && 'message' in body
					? String((body as { message: unknown }).message)
					: 'provider error'
			);
			this.name = 'ProviderError';
			this.body = body;
		}
	}
}));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
vi.mock('./deferredDelete.svelte', () => ({ deferredDelete: { flush: async () => {} } }));

import { classifyCsvUpdate, resolveCsvCanonicalAddresses } from './tagCsvDiff';
import { parseTagsCsv, TAG_CSV_COLUMNS, type ParsedCsvTagRow } from './tagCsv';
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';

// --- 共通フィクスチャ ---------------------------------------------------------

const CONN_A: PlcConnection = {
	id: 1,
	name: 'ConnA',
	protocol: 'modbus-tcp',
	enabled: true,
	simulation: false,
	settings: {}
};
// R2b-2d: 種類ごとの配置（db は postgres、computed は calc、internal は mem の下）。
const CONN_PG: PlcConnection = { ...CONN_A, id: 2, name: 'PgConn', protocol: 'postgres' };
const CONN_CALC: PlcConnection = { ...CONN_A, id: 3, name: 'calc', protocol: 'virtual' };
const CONN_MEM: PlcConnection = { ...CONN_A, id: 4, name: 'mem', protocol: 'virtual' };
const CONNECTIONS: PlcConnection[] = [CONN_A, CONN_PG, CONN_CALC, CONN_MEM];

const GROUP_A: CollectionGroup = {
	id: 10,
	name: 'GroupA',
	plcConnectionId: CONN_A.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true,
	querySql: null
};
const GROUP_B: CollectionGroup = {
	id: 11,
	name: 'GroupB',
	plcConnectionId: CONN_A.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true,
	querySql: null
};
const GROUP_PG: CollectionGroup = { ...GROUP_A, id: 12, name: 'GroupPg', plcConnectionId: 2 };
const GROUP_CALC: CollectionGroup = { ...GROUP_A, id: 13, name: 'GroupCalc', plcConnectionId: 3 };
const GROUP_MEM: CollectionGroup = { ...GROUP_A, id: 14, name: 'GroupMem', plcConnectionId: 4 };
const GROUPS: CollectionGroup[] = [GROUP_A, GROUP_B, GROUP_PG, GROUP_CALC, GROUP_MEM];

function makeTag(overrides: Partial<Tag> = {}): Tag {
	return {
		id: 1,
		name: 'Tag1',
		comment: '',
		collectionGroupId: GROUP_A.id,
		address: 'D100',
		dataType: 'i16',
		stringLength: null,
		stringEncoding: 'utf8',
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit: null,
		decimals: 0,
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
	stringEncoding: '',
	unit: '',
	decimals: '',
	rawLo: '',
	rawHi: '',
	engLo: '',
	engHi: '',
	enabled: '',
	writable: '',
	tagKind: '',
	expression: '',
	retain: '',
	comment: ''
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
	it('updates only the comment when the new CSV column changes', () => {
		const existing = makeTag({ comment: '旧メモ' });
		const result = classifyCsvUpdate(parsed([csvRow({ comment: '新メモ' })]), [existing]);
		expect(result.rows[0].diffs?.map((diff) => diff.field)).toEqual(['comment']);
		expect(result.updateRows[0].comment).toBe('新メモ');
	});

	it('keeps an existing comment when a legacy CSV changes another field', () => {
		const columns = TAG_CSV_COLUMNS.slice(0, -1);
		const oldRow = csvRow({ address: 'D200' });
		const csv = [columns.join(','), columns.map((column) => oldRow[column]).join(',')].join('\r\n');
		const parsedLegacy = parseTagsCsv(csv, CONNECTIONS, GROUPS);
		if (!parsedLegacy.ok) throw new Error(JSON.stringify(parsedLegacy.errors));
		const result = classifyCsvUpdate(parsedLegacy.rows, [makeTag({ comment: '保持' })]);
		expect(result.rows[0].diffs?.map((diff) => diff.field)).toEqual(['address']);
		expect(result.updateRows[0].comment).toBe('保持');
	});
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

	it('stringEncoding だけが変わった行も changed になる(T20 ①a-UI)', () => {
		const existing = makeTag({ dataType: 'string', stringLength: 8, stringEncoding: 'utf8' });
		const rows = parsed([
			csvRow({ dataType: 'string', stringLength: '8', stringEncoding: 'shift_jis' })
		]);
		const result = classifyCsvUpdate(rows, [existing]);
		expect(result.rows[0].category).toBe('changed');
		expect(result.rows[0].diffs).toEqual([
			{ field: 'stringEncoding', from: 'utf8', to: 'shift_jis' }
		]);
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
		// unit/rawLo/rawHi/engLo/engHi/stringLength は既存タグでは
		// null、CSV で空欄なら parseTagsCsv 側は undefined を返す
		// (tagCsv.ts の numField/col 実装)。この差だけで changed にならない
		// ことを固定する(toComparableRecord の正規化)。
		const existing = makeTag({
			unit: null,
			rawLo: null,
			rawHi: null,
			engLo: null,
			engHi: null,
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

// ==============================================================================
// アドレスの正規形での比較（R2b-2d）。解析の API は本物の HTTP ヘルパーを通し、
// `fetch` だけをモックする。
// ==============================================================================

/**
 * 解析の API の代役のサーバー（`fetch` のモック）。種類ごとの規則を、保存と同じ関数が
 * 返すはずの形で返す: `plc` は 5 桁の Modbus を 6 桁へ・`zzz` は断る、`db` は前後の空白を
 * 除く、`computed` / `internal` は空なら `""`・空でなければ断る。呼び出しの記録つき。
 */
function mockParseApi() {
	const calls: { connectionId: number; items: Record<string, unknown>[] }[] = [];
	globalThis.fetch = vi.fn(async (_url: string, init?: RequestInit) => {
		const body = JSON.parse(init?.body as string) as {
			connectionId: number;
			items: { tagKind: string; address: string }[];
		};
		calls.push(body);
		const items = body.items.map((item) => {
			const text = item.address.trim();
			if (item.tagKind === 'plc') {
				if (text === 'zzz') return { ok: false, message: 'address の形式が不正です' };
				const canonical = /^\d{5}$/.test(text) ? `${text[0]}0${text.slice(1)}` : text;
				return {
					ok: true,
					canonical,
					area: 'holding_register',
					writableArea: true,
					bit: null,
					wordCount: 1
				};
			}
			if (item.tagKind === 'db') {
				return {
					ok: true,
					canonical: text,
					area: null,
					writableArea: null,
					bit: null,
					wordCount: null
				};
			}
			return text === ''
				? { ok: true, canonical: '', area: null, writableArea: null, bit: null, wordCount: null }
				: { ok: false, message: `${item.tagKind} タグには address を設定できません` };
		});
		return new Response(JSON.stringify({ items }), { status: 200 });
	}) as never;
	return calls;
}

describe('正規形のアドレスでの比較（解析の API を通す）', () => {
	const originalFetch = globalThis.fetch;
	afterEach(() => {
		globalThis.fetch = originalFetch;
	});
	const groupToConnection = (groupId: number) =>
		GROUPS.find((g) => g.id === groupId)?.plcConnectionId;

	it('変更の無い CSV は plc(40001 vs 400001)・db・computed・internal のすべてが unchanged で error が 0', async () => {
		const calls = mockParseApi();
		const rows = parsed([
			csvRow({ name: 'P', address: '40001' }),
			csvRow({
				connection: 'PgConn',
				group: 'GroupPg',
				name: 'D',
				address: 'temperature',
				tagKind: 'db'
			}),
			csvRow({
				connection: 'calc',
				group: 'GroupCalc',
				name: 'C',
				address: '',
				tagKind: 'computed',
				expression: 'a + 1'
			}),
			csvRow({ connection: 'mem', group: 'GroupMem', name: 'I', address: '', tagKind: 'internal' })
		]);
		const existing = [
			makeTag({ id: 1, name: 'P', address: '400001' }),
			makeTag({
				id: 2,
				name: 'D',
				collectionGroupId: GROUP_PG.id,
				address: 'temperature',
				tagKind: 'db'
			}),
			makeTag({
				id: 3,
				name: 'C',
				collectionGroupId: GROUP_CALC.id,
				address: '',
				tagKind: 'computed',
				expression: 'a + 1'
			}),
			makeTag({
				id: 4,
				name: 'I',
				collectionGroupId: GROUP_MEM.id,
				address: '',
				tagKind: 'internal'
			})
		];

		const canonical = await resolveCsvCanonicalAddresses(rows, groupToConnection);
		const result = classifyCsvUpdate(rows, existing, canonical);

		expect(result.errorCount).toBe(0);
		expect(result.unchangedCount).toBe(4);
		expect(result.changedCount).toBe(0);
		expect(result.updateRows).toHaveLength(0);
		// 接続ごとに 1 回（4 接続 = 4 回）、全行が tagKind つきで送られる。
		expect(calls.map((c) => c.connectionId).sort()).toEqual([1, 2, 3, 4]);
		expect(calls.flatMap((c) => c.items.map((i) => i.tagKind)).sort()).toEqual([
			'computed',
			'db',
			'internal',
			'plc'
		]);
	});

	it('正規形に直さずに比べると 40001 の行は changed になる（解析の効果の対照）', () => {
		const rows = parsed([csvRow({ name: 'P', address: '40001' })]);
		const result = classifyCsvUpdate(rows, [makeTag({ id: 1, name: 'P', address: '400001' })]);
		expect(result.changedCount).toBe(1);
	});

	it('アドレスが本当に違えば、正規形でも changed で、送る行のアドレスは正規形', async () => {
		mockParseApi();
		const rows = parsed([csvRow({ name: 'P', address: '40002' })]);
		const canonical = await resolveCsvCanonicalAddresses(rows, groupToConnection);
		const result = classifyCsvUpdate(
			rows,
			[makeTag({ id: 1, name: 'P', address: '400001' })],
			canonical
		);
		expect(result.changedCount).toBe(1);
		expect(result.updateRows[0].address).toBe('400002');
	});

	it('解析に失敗した行はメッセージつきの error になる', async () => {
		mockParseApi();
		const rows = parsed([csvRow({ name: 'P', address: 'zzz' })]);
		const canonical = await resolveCsvCanonicalAddresses(rows, groupToConnection);
		const result = classifyCsvUpdate(
			rows,
			[makeTag({ id: 1, name: 'P', address: '400001' })],
			canonical
		);
		expect(result.errorCount).toBe(1);
		expect(result.rows[0].category).toBe('error');
		expect(result.rows[0].message).toBe('address の形式が不正です');
	});

	it('通信が失敗したら、その接続の全行が error（確認できなかったものを問題なしにしない）', async () => {
		globalThis.fetch = vi.fn(async () => {
			throw new Error('network down');
		}) as never;
		const rows = parsed([
			csvRow({ name: 'P', address: '40001' }),
			csvRow({ name: 'Q', address: '40002' })
		]);
		const canonical = await resolveCsvCanonicalAddresses(rows, groupToConnection);
		const result = classifyCsvUpdate(
			rows,
			[makeTag({ id: 1, name: 'P', address: '400001' })],
			canonical
		);
		expect(result.errorCount).toBe(2);
		expect(result.rows[0].message).toContain('確認できませんでした');
	});
});
