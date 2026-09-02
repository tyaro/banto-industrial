/**
 * H5: フロントテスト基盤（docs/improvement-plan.md）。`tagCsv.ts` の全公開
 * API（RFC4180 プリミティブ・タグCSVインポート/エクスポート）を対象とした
 * 網羅的ユニットテスト。`tagCsv.ts` 冒頭 docblock と各関数の docblock に
 * 書かれている設計判断（例: 不正な CSV でも例外を投げない、接続・グループ
 * は名前解決で自動作成しない、tagKind ごとの address/expression/retain の
 * 強制ルール）を仕様として固定する。
 *
 * フィクスチャの `connections`/`groups` は `GroupX` という同名グループが
 * 異なる2接続（ConnA/ConnB）の双方に存在する構成にしてあり、
 * 「グループ名は接続スコープで解決する」ことを実際に区別できるようにして
 * いる。
 */
import { describe, expect, it, test } from 'vitest';
import {
	buildErrorRowsCsv,
	buildTagCsvTemplate,
	checkCsvRowLimit,
	checkCsvSizeLimit,
	exportTagsCsv,
	MAX_CSV_BYTES,
	MAX_CSV_ROWS,
	parseCsv,
	parseTagsCsv,
	serializeCsv,
	stripBom,
	TAG_CSV_COLUMNS,
	type CsvRowError,
	type ImportTagsCsvResult,
	type ParsedCsvTagRow
} from './tagCsv';
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
const CONN_B: PlcConnection = {
	id: 2,
	name: 'ConnB',
	protocol: 'slmp',
	host: '127.0.0.2',
	port: 5000,
	unitId: 1,
	enabled: true,
	simulation: false,
	wordOrder: 'low_high'
};
const CONNECTIONS: PlcConnection[] = [CONN_A, CONN_B];

// 同名グループ「GroupX」を ConnA/ConnB の双方に置き、接続スコープでの名前
// 解決を区別できるようにする。
const GROUP_A_X: CollectionGroup = {
	id: 10,
	name: 'GroupX',
	plcConnectionId: CONN_A.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true
};
const GROUP_A_Y: CollectionGroup = {
	id: 11,
	name: 'GroupY',
	plcConnectionId: CONN_A.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true
};
const GROUP_B_X: CollectionGroup = {
	id: 20,
	name: 'GroupX',
	plcConnectionId: CONN_B.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true
};
const GROUPS: CollectionGroup[] = [GROUP_A_X, GROUP_A_Y, GROUP_B_X];

function makeTag(overrides: Partial<Tag> = {}): Tag {
	return {
		id: 1,
		name: 'Tag1',
		collectionGroupId: GROUP_A_X.id,
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

// --- タグCSV行の組み立てヘルパ -------------------------------------------------

type CsvColumn = (typeof TAG_CSV_COLUMNS)[number];
type CsvRowFields = Record<CsvColumn, string>;

/** 妥当な最小 plc タグ1行分の既定値。空欄にしたい列だけ上書きして使う。 */
const DEFAULT_ROW: CsvRowFields = {
	connection: 'ConnA',
	group: 'GroupX',
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

function row(overrides: Partial<CsvRowFields> = {}): string[] {
	const merged: CsvRowFields = { ...DEFAULT_ROW, ...overrides };
	return TAG_CSV_COLUMNS.map((c) => merged[c]);
}

/** 1行分のセル配列を、引用要否を自動判定する1行の CSV テキスト(改行なし)にする。 */
function csvLine(cells: string[]): string {
	return serializeCsv([cells]).replace(/\r\n$/, '');
}

/** ヘッダ行 + データ行から CSV 全文を組み立てる。空行を混ぜたい場合は
 *  `extraLines` にそのまま渡す(既に整形済みの行として結合される)。 */
function buildCsv(dataRows: string[][], opts: { bom?: boolean; header?: string[] } = {}): string {
	const header = opts.header ?? [...TAG_CSV_COLUMNS];
	const lines = [csvLine(header), ...dataRows.map(csvLine)];
	return (opts.bom ? '﻿' : '') + lines.join('\r\n') + '\r\n';
}

function expectOk(result: ImportTagsCsvResult): ParsedCsvTagRow[] {
	if (!result.ok) {
		throw new Error(`ok:true を期待したが errors: ${JSON.stringify(result.errors)}`);
	}
	return result.rows;
}

function expectErr(result: ImportTagsCsvResult): CsvRowError[] {
	if (result.ok) {
		throw new Error('ok:false（エラー）を期待したが ok:true が返った');
	}
	return result.errors;
}

// ==============================================================================
// stripBom
// ==============================================================================

describe('stripBom', () => {
	it('先頭の BOM を除去する', () => {
		expect(stripBom('﻿abc')).toBe('abc');
	});

	it('BOM が無ければそのまま通す', () => {
		expect(stripBom('abc')).toBe('abc');
	});

	it('空文字列はそのまま空文字列', () => {
		expect(stripBom('')).toBe('');
	});

	it('BOM のみの文字列は空文字列になる', () => {
		expect(stripBom('﻿')).toBe('');
	});

	it('文中に現れる U+FEFF は除去しない(先頭のみ対象)', () => {
		expect(stripBom('a﻿b')).toBe('a﻿b');
	});
});

// ==============================================================================
// parseCsv
// ==============================================================================

describe('parseCsv', () => {
	describe('基本', () => {
		it('単一行', () => {
			expect(parseCsv('a,b,c')).toEqual([['a', 'b', 'c']]);
		});

		it('複数行', () => {
			expect(parseCsv('a,b\nc,d')).toEqual([
				['a', 'b'],
				['c', 'd']
			]);
		});

		it('空フィールドを含む行', () => {
			expect(parseCsv('a,,c')).toEqual([['a', '', 'c']]);
		});

		it('行末に改行がある場合、余分な空行を作らない', () => {
			expect(parseCsv('a,b\n')).toEqual([['a', 'b']]);
		});

		it('行末に改行が無い場合も最終行を読み取る', () => {
			expect(parseCsv('a,b')).toEqual([['a', 'b']]);
		});

		it('空文字列の入力は空配列を返す', () => {
			expect(parseCsv('')).toEqual([]);
		});
	});

	describe('改行コード', () => {
		it('LF 区切り', () => {
			expect(parseCsv('a,b\nc,d\ne,f')).toEqual([
				['a', 'b'],
				['c', 'd'],
				['e', 'f']
			]);
		});

		it('CRLF 区切り', () => {
			expect(parseCsv('a,b\r\nc,d\r\ne,f')).toEqual([
				['a', 'b'],
				['c', 'd'],
				['e', 'f']
			]);
		});

		it('LF/CRLF 混在', () => {
			expect(parseCsv('a,b\r\nc,d\ne,f\r\n')).toEqual([
				['a', 'b'],
				['c', 'd'],
				['e', 'f']
			]);
		});

		it('フィールド内埋め込み改行(LF)', () => {
			expect(parseCsv('"a\nb",c')).toEqual([['a\nb', 'c']]);
		});

		it('フィールド内埋め込み改行(CRLF)', () => {
			expect(parseCsv('"a\r\nb",c')).toEqual([['a\r\nb', 'c']]);
		});
	});

	describe('引用符', () => {
		it('引用符付きフィールド', () => {
			expect(parseCsv('"a",b')).toEqual([['a', 'b']]);
		});

		it('埋め込みカンマを含む引用符付きフィールド', () => {
			expect(parseCsv('"a,b",c')).toEqual([['a,b', 'c']]);
		});

		it('"" によるエスケープ(引用符自体を含むフィールド)', () => {
			expect(parseCsv('"a""b",c')).toEqual([['a"b', 'c']]);
		});

		it('引用符+カンマ+改行の複合フィールド', () => {
			expect(parseCsv('"a,b\nc",d')).toEqual([['a,b\nc', 'd']]);
		});

		it('引用符が閉じた直後にカンマが続く', () => {
			expect(parseCsv('"a",b,"c"')).toEqual([['a', 'b', 'c']]);
		});

		it('引用符が閉じた直後に改行が続く', () => {
			expect(parseCsv('"a"\n"b"')).toEqual([['a'], ['b']]);
		});

		it('引用符が閉じた直後に EOF が続く', () => {
			expect(parseCsv('"a"')).toEqual([['a']]);
		});
	});

	describe('不正入力でも例外を投げない(未終端の引用符)', () => {
		it('未終端の引用符は、以降のカンマも含めて全文字をフィールド内容として取り込む', () => {
			expect(() => parseCsv('"a,b')).not.toThrow();
			expect(parseCsv('"a,b')).toEqual([['a,b']]);
		});

		it('未終端の引用符は、以降の改行も含めて全文字をフィールド内容として取り込み末尾で1行確定する', () => {
			expect(() => parseCsv('"a,b\nc')).not.toThrow();
			expect(parseCsv('"a,b\nc')).toEqual([['a,b\nc']]);
		});
	});

	describe('空行の扱い', () => {
		it('ファイル中間の空行は1セル(空文字)の行として現れる', () => {
			expect(parseCsv('a,b\n\nc,d')).toEqual([['a', 'b'], [''], ['c', 'd']]);
		});

		it('ファイル末尾の改行1個では余分な最終行を生成しない', () => {
			expect(parseCsv('a,b\n\nc,d\n')).toEqual([['a', 'b'], [''], ['c', 'd']]);
		});

		it('空フィールド1個だけの行を明示的に書けば1セルの行として現れる', () => {
			expect(parseCsv('\r\n')).toEqual([['']]);
		});
	});

	describe('フィールド途中に引用符が現れるケース(現実装の挙動を固定)', () => {
		it('非引用フィールドの途中に " が現れると、以降(カンマ含む)を引用モードとして取り込む', () => {
			// 'a' → field='a'。'"' で inQuotes=true になり、以降の 'b' ',' 'c' は
			// 区切り文字として扱われず、閉じる引用符も無いためそのまま EOF まで
			// フィールド内容として取り込まれる。
			expect(parseCsv('a"b,c')).toEqual([['ab,c']]);
		});

		it('引用モード中に閉じる " が見つかれば、以降はまた非引用として区切り文字が効く', () => {
			expect(parseCsv('a"b"c,d')).toEqual([['abc', 'd']]);
		});
	});
});

// ==============================================================================
// serializeCsv
// ==============================================================================

describe('serializeCsv', () => {
	describe('基本', () => {
		it('空配列は空文字列', () => {
			expect(serializeCsv([])).toBe('');
		});

		it('通常行は CRLF 区切り、かつ末尾にも CRLF が付く', () => {
			expect(
				serializeCsv([
					['a', 'b'],
					['c', 'd']
				])
			).toBe('a,b\r\nc,d\r\n');
		});
	});

	describe('引用ルール', () => {
		it('カンマを含むフィールドは引用される', () => {
			expect(serializeCsv([['a,b']])).toBe('"a,b"\r\n');
		});

		it('引用符を含むフィールドは引用され、内部の " は "" に二重化される', () => {
			expect(serializeCsv([['a"b']])).toBe('"a""b"\r\n');
		});

		it('\\n を含むフィールドは引用される', () => {
			expect(serializeCsv([['a\nb']])).toBe('"a\nb"\r\n');
		});

		it('\\r を含むフィールドは引用される', () => {
			expect(serializeCsv([['a\rb']])).toBe('"a\rb"\r\n');
		});

		it('カンマ・引用符・改行のいずれも含まないフィールドは引用されない', () => {
			expect(serializeCsv([['abc']])).toBe('abc\r\n');
		});

		it('日本語はそのまま出力される(引用不要)', () => {
			expect(serializeCsv([['日本語のタグ名']])).toBe('日本語のタグ名\r\n');
		});
	});

	describe('ラウンドトリップ: parseCsv(serializeCsv(rows)) === rows', () => {
		const cases: Array<[string, string[][]]> = [
			['埋め込みカンマ', [['a,b', 'c']]],
			['埋め込み引用符', [['a"b', 'c']]],
			['埋め込み改行(LF)', [['a\nb', 'c']]],
			['日本語', [['接続A', 'グループ1', 'タグ名']]],
			[
				'空フィールドを含む複数行',
				[
					['a', '', 'c'],
					['', '', '']
				]
			],
			[
				'複合(カンマ+引用符+改行+日本語混在)',
				[
					['接続,A', 'タグ"1"', '複数\n行\r\nコメント'],
					['', '日本語', '123']
				]
			]
		];

		for (const [label, rows] of cases) {
			it(label, () => {
				expect(parseCsv(serializeCsv(rows))).toEqual(rows);
			});
		}
	});
});

// ==============================================================================
// TAG_CSV_COLUMNS
// ==============================================================================

describe('TAG_CSV_COLUMNS', () => {
	it('21 列である', () => {
		expect(TAG_CSV_COLUMNS.length).toBe(21);
	});

	it('列順のスナップショット', () => {
		expect([...TAG_CSV_COLUMNS]).toEqual([
			'connection',
			'group',
			'name',
			'address',
			'dataType',
			'stringLength',
			'unit',
			'decimals',
			'rawLo',
			'rawHi',
			'engLo',
			'engHi',
			'thresholdH',
			'thresholdHh',
			'thresholdL',
			'thresholdLl',
			'enabled',
			'writable',
			'tagKind',
			'expression',
			'retain'
		]);
	});
});

// ==============================================================================
// exportTagsCsv
// ==============================================================================

describe('exportTagsCsv', () => {
	it('先頭に UTF-8 BOM(U+FEFF)が付く', () => {
		const csv = exportTagsCsv([], CONNECTIONS, GROUPS);
		expect(csv.charCodeAt(0)).toBe(0xfeff);
	});

	it('1行目がヘッダ行で TAG_CSV_COLUMNS どおり', () => {
		const csv = exportTagsCsv([], CONNECTIONS, GROUPS);
		const [header] = parseCsv(stripBom(csv));
		expect(header).toEqual([...TAG_CSV_COLUMNS]);
	});

	it('タグ 0 本 -> ヘッダ行のみ', () => {
		const csv = exportTagsCsv([], CONNECTIONS, GROUPS);
		expect(stripBom(csv)).toBe(TAG_CSV_COLUMNS.join(',') + '\r\n');
	});

	it('数値欄が null/undefined のフィールドは空欄になる', () => {
		const tag = makeTag({
			stringLength: null,
			rawLo: null,
			rawHi: null,
			engLo: null,
			engHi: null,
			thresholdH: null,
			thresholdHh: null,
			thresholdL: null,
			thresholdLl: null
		});
		const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
		const [, dataRow] = parseCsv(stripBom(csv));
		const idx = (c: CsvColumn) => TAG_CSV_COLUMNS.indexOf(c);
		for (const c of [
			'stringLength',
			'rawLo',
			'rawHi',
			'engLo',
			'engHi',
			'thresholdH',
			'thresholdHh',
			'thresholdL',
			'thresholdLl'
		] as const) {
			expect(dataRow[idx(c)]).toBe('');
		}
	});

	it('boolean 欄は "true"/"false" の文字列になる', () => {
		const tag = makeTag({ enabled: false, writable: true, retain: true, tagKind: 'internal' });
		const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
		const [, dataRow] = parseCsv(stripBom(csv));
		const idx = (c: CsvColumn) => TAG_CSV_COLUMNS.indexOf(c);
		expect(dataRow[idx('enabled')]).toBe('false');
		expect(dataRow[idx('writable')]).toBe('true');
		expect(dataRow[idx('retain')]).toBe('true');
	});

	it('unit/expression が null のフィールドは空欄になる', () => {
		const tag = makeTag({ unit: null, expression: null });
		const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
		const [, dataRow] = parseCsv(stripBom(csv));
		const idx = (c: CsvColumn) => TAG_CSV_COLUMNS.indexOf(c);
		expect(dataRow[idx('unit')]).toBe('');
		expect(dataRow[idx('expression')]).toBe('');
	});

	it('decimals は数値欄と違い、0 でも空欄にはならない', () => {
		const tag = makeTag({ decimals: 0 });
		const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
		const [, dataRow] = parseCsv(stripBom(csv));
		expect(dataRow[TAG_CSV_COLUMNS.indexOf('decimals')]).toBe('0');
	});

	it('グループ→接続の名前を解決して connection/group 列に出す', () => {
		const tag = makeTag({ collectionGroupId: GROUP_B_X.id });
		const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
		const [, dataRow] = parseCsv(stripBom(csv));
		expect(dataRow[TAG_CSV_COLUMNS.indexOf('connection')]).toBe('ConnB');
		expect(dataRow[TAG_CSV_COLUMNS.indexOf('group')]).toBe('GroupX');
	});

	it('groupInfo に無い collectionGroupId は connection/group 列が空欄になる', () => {
		const tag = makeTag({ collectionGroupId: 999999 });
		const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
		const [, dataRow] = parseCsv(stripBom(csv));
		expect(dataRow[TAG_CSV_COLUMNS.indexOf('connection')]).toBe('');
		expect(dataRow[TAG_CSV_COLUMNS.indexOf('group')]).toBe('');
	});

	it('グループはあるが対応する接続が見つからない場合、connectionName だけ空欄になる', () => {
		const orphanGroup: CollectionGroup = {
			id: 30,
			name: 'OrphanGroup',
			plcConnectionId: 999999,
			periodMs: 1000,
			enabled: true,
			defaultWritable: true
		};
		const tag = makeTag({ collectionGroupId: orphanGroup.id });
		const csv = exportTagsCsv([tag], CONNECTIONS, [...GROUPS, orphanGroup]);
		const [, dataRow] = parseCsv(stripBom(csv));
		expect(dataRow[TAG_CSV_COLUMNS.indexOf('connection')]).toBe('');
		expect(dataRow[TAG_CSV_COLUMNS.indexOf('group')]).toBe('OrphanGroup');
	});

	describe('エクスポート -> parseTagsCsv のラウンドトリップ', () => {
		it('plc タグ(数値欄・単位・引用が必要な名前を含む)', () => {
			const tag = makeTag({
				id: 100,
				name: 'Full,Tag "A"',
				collectionGroupId: GROUP_A_X.id,
				address: 'D200',
				dataType: 'f32',
				rawLo: 0,
				rawHi: 4095,
				engLo: -10.5,
				engHi: 99.9,
				unit: '℃',
				decimals: 2,
				thresholdH: 80,
				thresholdHh: 90,
				thresholdL: 10,
				thresholdLl: 5,
				enabled: true,
				writable: true,
				tagKind: 'plc'
			});
			const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
			const result = parseTagsCsv(csv, CONNECTIONS, GROUPS);
			const rows = expectOk(result);
			expect(rows).toHaveLength(1);
			expect(rows[0].tag).toEqual({
				name: 'Full,Tag "A"',
				collectionGroupId: GROUP_A_X.id,
				address: 'D200',
				dataType: 'f32',
				stringLength: undefined,
				rawLo: 0,
				rawHi: 4095,
				engLo: -10.5,
				engHi: 99.9,
				unit: '℃',
				decimals: 2,
				thresholdH: 80,
				thresholdHh: 90,
				thresholdL: 10,
				thresholdLl: 5,
				enabled: true,
				writable: true,
				tagKind: 'plc',
				expression: undefined,
				retain: false
			});
		});

		it('string タグ', () => {
			const tag = makeTag({
				id: 101,
				name: 'StrTag',
				collectionGroupId: GROUP_A_Y.id,
				address: 'D300',
				dataType: 'string',
				stringLength: 16,
				decimals: 0
			});
			const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
			const rows = expectOk(parseTagsCsv(csv, CONNECTIONS, GROUPS));
			expect(rows[0].tag.dataType).toBe('string');
			expect(rows[0].tag.stringLength).toBe(16);
		});

		it('computed タグ(address/writable が強制される)', () => {
			const tag = makeTag({
				id: 102,
				name: 'CalcTag',
				collectionGroupId: GROUP_B_X.id,
				address: '',
				dataType: 'f32',
				decimals: 1,
				tagKind: 'computed',
				expression: 'a + b',
				writable: false
			});
			const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
			const rows = expectOk(parseTagsCsv(csv, CONNECTIONS, GROUPS));
			expect(rows[0].tag.tagKind).toBe('computed');
			expect(rows[0].tag.expression).toBe('a + b');
			expect(rows[0].tag.address).toBe('');
			expect(rows[0].tag.writable).toBe(false);
		});

		it('internal タグ(retain が保存される)', () => {
			const tag = makeTag({
				id: 103,
				name: 'IntTag',
				collectionGroupId: GROUP_A_X.id,
				address: '',
				dataType: 'bit',
				decimals: 0,
				tagKind: 'internal',
				retain: true
			});
			const csv = exportTagsCsv([tag], CONNECTIONS, GROUPS);
			const rows = expectOk(parseTagsCsv(csv, CONNECTIONS, GROUPS));
			expect(rows[0].tag.tagKind).toBe('internal');
			expect(rows[0].tag.retain).toBe(true);
			expect(rows[0].tag.address).toBe('');
		});
	});
});

// ==============================================================================
// parseTagsCsv
// ==============================================================================

describe('parseTagsCsv', () => {
	describe('空入力', () => {
		it('空文字列 -> {ok:true, rows:[]}', () => {
			expect(parseTagsCsv('', CONNECTIONS, GROUPS)).toEqual({ ok: true, rows: [] });
		});

		it('空白のみ -> {ok:true, rows:[]}', () => {
			expect(parseTagsCsv('   \n\t \n', CONNECTIONS, GROUPS)).toEqual({ ok: true, rows: [] });
		});

		it('BOM のみ -> {ok:true, rows:[]}', () => {
			expect(parseTagsCsv('﻿', CONNECTIONS, GROUPS)).toEqual({ ok: true, rows: [] });
		});

		it('BOM + 空白のみ -> {ok:true, rows:[]}', () => {
			expect(parseTagsCsv('﻿   \n  ', CONNECTIONS, GROUPS)).toEqual({ ok: true, rows: [] });
		});
	});

	describe('ヘッダ', () => {
		it('ヘッダのみ(データ行無し) -> {ok:true, rows:[]}', () => {
			expect(parseTagsCsv(buildCsv([]), CONNECTIONS, GROUPS)).toEqual({ ok: true, rows: [] });
		});

		it('BOM 付きヘッダを受理し、後続のデータ行も正しくパースする', () => {
			const text = buildCsv([row()], { bom: true });
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows).toHaveLength(1);
		});

		it('列名が1つでも違うとヘッダ不一致エラー(lineNumber:1)', () => {
			const header: string[] = [...TAG_CSV_COLUMNS];
			header[0] = 'conn';
			const text = buildCsv([row()], { header });
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toHaveLength(1);
			expect(errors[0].lineNumber).toBe(1);
		});

		it('列数が少ないとヘッダ不一致エラー(lineNumber:1)', () => {
			const header = TAG_CSV_COLUMNS.slice(0, -1) as unknown as string[];
			const text = buildCsv([], { header });
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors[0].lineNumber).toBe(1);
		});

		it('列数が多いとヘッダ不一致エラー(lineNumber:1)', () => {
			const header = [...TAG_CSV_COLUMNS, 'extra'];
			const text = buildCsv([], { header });
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors[0].lineNumber).toBe(1);
		});

		it('列の順序が違うとヘッダ不一致エラー(lineNumber:1)', () => {
			const header: string[] = [...TAG_CSV_COLUMNS];
			[header[0], header[1]] = [header[1], header[0]];
			const text = buildCsv([], { header });
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors[0].lineNumber).toBe(1);
		});
	});

	describe('正常系', () => {
		it('正常な最小行(plc タグ)を1行パースする', () => {
			const text = buildCsv([row()]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows).toHaveLength(1);
			expect(rows[0]).toEqual({
				lineNumber: 2,
				connectionName: 'ConnA',
				groupName: 'GroupX',
				tag: {
					name: 'Tag1',
					collectionGroupId: GROUP_A_X.id,
					address: 'D100',
					dataType: 'i16',
					stringLength: undefined,
					rawLo: undefined,
					rawHi: undefined,
					engLo: undefined,
					engHi: undefined,
					unit: undefined,
					decimals: 0,
					thresholdH: undefined,
					thresholdHh: undefined,
					thresholdL: undefined,
					thresholdLl: undefined,
					enabled: true,
					writable: false,
					tagKind: 'plc',
					expression: undefined,
					retain: false
				}
			});
		});

		it('複数行は lineNumber が 2 起点で連番になる', () => {
			const text = buildCsv([row({ name: 'Tag1' }), row({ name: 'Tag2' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows.map((r) => r.lineNumber)).toEqual([2, 3]);
		});

		it('ファイル中間の空行はスキップされ、以降の行番号はズレない', () => {
			const text =
				[
					csvLine([...TAG_CSV_COLUMNS]),
					csvLine(row({ name: 'Tag1' })),
					'',
					csvLine(row({ name: 'Tag2' }))
				].join('\r\n') + '\r\n';
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows.map((r) => r.lineNumber)).toEqual([2, 4]);
			expect(rows.map((r) => r.tag.name)).toEqual(['Tag1', 'Tag2']);
		});
	});

	describe('必須欄', () => {
		it('connection が空 -> エラー', () => {
			const text = buildCsv([row({ connection: '' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({ lineNumber: 2, message: 'connection は必須です。' });
		});

		it('group が空 -> エラー', () => {
			const text = buildCsv([row({ group: '' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({ lineNumber: 2, message: 'group は必須です。' });
		});

		it('name が空 -> エラー', () => {
			const text = buildCsv([row({ name: '' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({ lineNumber: 2, message: 'name は必須です。' });
		});
	});

	describe('接続・グループの名前解決', () => {
		it('接続名が見つからない -> エラー', () => {
			const text = buildCsv([row({ connection: 'NoSuchConn' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: '接続 "NoSuchConn" が見つかりません。'
			});
		});

		it('グループ名が(その接続内では)見つからない -> エラー(別接続に同名グループがあっても不一致)', () => {
			// GroupY は ConnA にしか無いので、connection=ConnB を指定すると解決できない。
			const text = buildCsv([row({ connection: 'ConnB', group: 'GroupY' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: 'グループ "GroupY"（接続 "ConnB"）が見つかりません。'
			});
		});

		it('同名グループが複数接続にある場合、connection 列で指定した接続配下のグループが選ばれる', () => {
			const text = buildCsv([row({ connection: 'ConnB', group: 'GroupX' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.collectionGroupId).toBe(GROUP_B_X.id);
		});
	});

	describe('dataType', () => {
		it('不正な値はエラー', () => {
			const text = buildCsv([row({ dataType: 'xyz' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({ lineNumber: 2, message: 'dataType "xyz" は不正な値です。' });
		});

		test.each(['bit', 'i16', 'u16', 'i32', 'u32', 'f32'] as const)('%s は受理される', (dt) => {
			const text = buildCsv([row({ dataType: dt })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.dataType).toBe(dt);
		});

		it('string は stringLength とあわせて受理される', () => {
			const text = buildCsv([row({ dataType: 'string', stringLength: '8' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.dataType).toBe('string');
			expect(rows[0].tag.stringLength).toBe(8);
		});
	});

	describe('tagKind', () => {
		it('空欄は既定値 "plc" になる', () => {
			const text = buildCsv([row({ tagKind: '' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.tagKind).toBe('plc');
		});

		it('"plc" を受理する', () => {
			const text = buildCsv([row({ tagKind: 'plc' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.tagKind).toBe('plc');
		});

		it('"computed" を受理する(expression 必須)', () => {
			const text = buildCsv([row({ tagKind: 'computed', expression: 'a+b' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.tagKind).toBe('computed');
		});

		it('"internal" を受理する', () => {
			const text = buildCsv([row({ tagKind: 'internal' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.tagKind).toBe('internal');
		});

		it('不正な値はエラー', () => {
			const text = buildCsv([row({ tagKind: 'foo' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({ lineNumber: 2, message: 'tagKind "foo" は不正な値です。' });
		});
	});

	describe('address', () => {
		it('tagKind=plc で空欄 -> エラー', () => {
			const text = buildCsv([row({ address: '' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: 'address は tagKind=plc のとき必須です。'
			});
		});

		it('computed/internal では CSV に何が書いてあっても "" に強制される', () => {
			const textComputed = buildCsv([
				row({ tagKind: 'computed', address: 'D999', expression: 'a+b' })
			]);
			const rowsComputed = expectOk(parseTagsCsv(textComputed, CONNECTIONS, GROUPS));
			expect(rowsComputed[0].tag.address).toBe('');

			const textInternal = buildCsv([row({ tagKind: 'internal', address: 'D999' })]);
			const rowsInternal = expectOk(parseTagsCsv(textInternal, CONNECTIONS, GROUPS));
			expect(rowsInternal[0].tag.address).toBe('');
		});
	});

	describe('stringLength', () => {
		it('dataType=string で空欄 -> エラー', () => {
			const text = buildCsv([row({ dataType: 'string', stringLength: '' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: 'stringLength は dataType=string のとき1以上の整数が必要です。'
			});
		});

		it('dataType=string で非整数 -> エラー', () => {
			const text = buildCsv([row({ dataType: 'string', stringLength: '1.5' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: 'stringLength は dataType=string のとき1以上の整数が必要です。'
			});
		});

		it('dataType=string で 0 以下 -> エラー', () => {
			const text = buildCsv([row({ dataType: 'string', stringLength: '0' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: 'stringLength は dataType=string のとき1以上の整数が必要です。'
			});
		});

		it('dataType=string で正整数 -> 受理される', () => {
			const text = buildCsv([row({ dataType: 'string', stringLength: '32' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.stringLength).toBe(32);
		});

		it('dataType が string 以外の場合は列内容にかかわらず undefined になる', () => {
			const text = buildCsv([row({ dataType: 'i16', stringLength: 'garbage' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.stringLength).toBeUndefined();
		});
	});

	describe('decimals', () => {
		it('空欄 -> 0', () => {
			const text = buildCsv([row({ decimals: '' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.decimals).toBe(0);
		});

		it('数値を指定すると反映される', () => {
			const text = buildCsv([row({ decimals: '3' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.decimals).toBe(3);
		});

		it('非数値はエラー', () => {
			const text = buildCsv([row({ decimals: 'abc' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: 'decimals は数値で指定してください。'
			});
		});
	});

	describe('数値欄(rawLo/rawHi/engLo/engHi/thresholdH/Hh/L/Ll)', () => {
		const NUMERIC_FIELDS = [
			'rawLo',
			'rawHi',
			'engLo',
			'engHi',
			'thresholdH',
			'thresholdHh',
			'thresholdL',
			'thresholdLl'
		] as const;

		it('すべて空欄なら undefined になる', () => {
			const text = buildCsv([row()]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			for (const f of NUMERIC_FIELDS) {
				expect(rows[0].tag[f]).toBeUndefined();
			}
		});

		it('数値を指定すればそれぞれ反映される', () => {
			const overrides: Partial<CsvRowFields> = {
				rawLo: '0',
				rawHi: '4095',
				engLo: '-5.5',
				engHi: '200',
				thresholdH: '80',
				thresholdHh: '90',
				thresholdL: '10',
				thresholdLl: '5'
			};
			const text = buildCsv([row(overrides)]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.rawLo).toBe(0);
			expect(rows[0].tag.rawHi).toBe(4095);
			expect(rows[0].tag.engLo).toBe(-5.5);
			expect(rows[0].tag.engHi).toBe(200);
			expect(rows[0].tag.thresholdH).toBe(80);
			expect(rows[0].tag.thresholdHh).toBe(90);
			expect(rows[0].tag.thresholdL).toBe(10);
			expect(rows[0].tag.thresholdLl).toBe(5);
		});

		test.each(NUMERIC_FIELDS)('%s に非数値を入れるとラベル付きエラーになる', (field) => {
			const text = buildCsv([row({ [field]: 'abc' } as Partial<CsvRowFields>)]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: `${field} は数値で指定してください。`
			});
		});
	});

	describe('unit', () => {
		it('空欄 -> undefined', () => {
			const text = buildCsv([row({ unit: '' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.unit).toBeUndefined();
		});

		it('値があればそのまま反映される', () => {
			const text = buildCsv([row({ unit: '℃' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.unit).toBe('℃');
		});
	});

	describe('boolean 欄(enabled/writable/retain)', () => {
		it('すべて空欄なら既定値になる(enabled:true, writable:false, retain:false)', () => {
			const text = buildCsv([row({ enabled: '', writable: '', retain: '', tagKind: 'internal' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.enabled).toBe(true);
			expect(rows[0].tag.writable).toBe(false);
			expect(rows[0].tag.retain).toBe(false);
		});

		test.each(['true', 'false', 'TRUE', 'False', '1', '0'] as const)(
			'enabled は "%s" を受理する',
			(v) => {
				const text = buildCsv([row({ enabled: v })]);
				const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
				const expected = v.toLowerCase() === 'true' || v === '1';
				expect(rows[0].tag.enabled).toBe(expected);
			}
		);

		it('writable も true/false 表記を受理する', () => {
			const text = buildCsv([row({ writable: 'true' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.writable).toBe(true);
		});

		it('retain(tagKind=internal) も true/false 表記を受理する', () => {
			const text = buildCsv([row({ tagKind: 'internal', retain: 'true' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.retain).toBe(true);
		});

		test.each(['enabled', 'writable', 'retain'] as const)('%s の不正値はエラー', (field) => {
			const text = buildCsv([row({ [field]: 'yes' } as Partial<CsvRowFields>)]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: `${field} は true/false（または1/0）で指定してください。`
			});
		});
	});

	describe('expression', () => {
		it('tagKind=computed で空欄 -> エラー', () => {
			const text = buildCsv([row({ tagKind: 'computed', expression: '' })]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(errors).toContainEqual({
				lineNumber: 2,
				message: 'expression は tagKind=computed のとき必須です。'
			});
		});

		it('tagKind=computed で値あり -> 受理される', () => {
			const text = buildCsv([row({ tagKind: 'computed', expression: 'a * 2 + 1' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.expression).toBe('a * 2 + 1');
		});

		it('tagKind=plc/internal では expression 列の内容は無視され undefined になる', () => {
			const textPlc = buildCsv([row({ tagKind: 'plc', expression: 'ignored' })]);
			expect(
				expectOk(parseTagsCsv(textPlc, CONNECTIONS, GROUPS))[0].tag.expression
			).toBeUndefined();

			const textInternal = buildCsv([row({ tagKind: 'internal', expression: 'ignored' })]);
			expect(
				expectOk(parseTagsCsv(textInternal, CONNECTIONS, GROUPS))[0].tag.expression
			).toBeUndefined();
		});
	});

	describe('retain', () => {
		it('tagKind=internal のときだけ有効になる', () => {
			const text = buildCsv([row({ tagKind: 'internal', retain: 'true' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.retain).toBe(true);
		});

		test.each(['plc', 'computed'] as const)(
			'tagKind=%s では retain=true と書いても false に強制される',
			(tagKind) => {
				const overrides: Partial<CsvRowFields> = { tagKind, retain: 'true' };
				if (tagKind === 'computed') overrides.expression = 'a+b';
				const text = buildCsv([row(overrides)]);
				const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
				expect(rows[0].tag.retain).toBe(false);
			}
		);
	});

	describe('writable', () => {
		it('tagKind=computed では writable=true と書いても false に強制される', () => {
			const text = buildCsv([row({ tagKind: 'computed', expression: 'a+b', writable: 'true' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.writable).toBe(false);
		});
	});

	describe('複数行にまたがるエラーの収集', () => {
		it('最初の不正行で止めず、全行分のエラーを収集する', () => {
			const text = buildCsv([
				row({ name: '' }), // line 2: name 必須エラー
				row({ dataType: 'bogus' }), // line 3: dataType 不正エラー
				row({ name: 'OkTag' }) // line 4: 正常行
			]);
			const errors = expectErr(parseTagsCsv(text, CONNECTIONS, GROUPS));
			const lineNumbers = errors.map((e) => e.lineNumber);
			expect(lineNumbers).toContain(2);
			expect(lineNumbers).toContain(3);
			expect(lineNumbers).not.toContain(4);
		});

		it('1件でもエラーがあれば ok:false になり、rows は返らない(全行不可)', () => {
			const text = buildCsv([row({ name: 'OkTag' }), row({ name: '' })]);
			const result = parseTagsCsv(text, CONNECTIONS, GROUPS);
			expect(result.ok).toBe(false);
			expect('rows' in result).toBe(false);
		});
	});

	describe('引用符付きフィールド', () => {
		it('埋め込みカンマや引用符を含むタグ名も正しく通る', () => {
			const text = buildCsv([row({ name: 'Tag, "special"' })]);
			const rows = expectOk(parseTagsCsv(text, CONNECTIONS, GROUPS));
			expect(rows[0].tag.name).toBe('Tag, "special"');
		});
	});
});

// ==============================================================================
// buildTagCsvTemplate (T18-3d)
// ==============================================================================

describe('buildTagCsvTemplate', () => {
	it('先頭に UTF-8 BOM(U+FEFF)が付く', () => {
		expect(buildTagCsvTemplate().charCodeAt(0)).toBe(0xfeff);
	});

	it('ヘッダ行のみ(データ行なし)で TAG_CSV_COLUMNS どおり', () => {
		const template = buildTagCsvTemplate();
		const table = parseCsv(stripBom(template));
		expect(table).toEqual([[...TAG_CSV_COLUMNS]]);
	});

	it('parseTagsCsv にそのまま通せる(データ行0件として受理される)', () => {
		const template = buildTagCsvTemplate();
		expect(parseTagsCsv(template, CONNECTIONS, GROUPS)).toEqual({ ok: true, rows: [] });
	});
});

// ==============================================================================
// buildErrorRowsCsv (T18-3d)
// ==============================================================================

describe('buildErrorRowsCsv', () => {
	it('先頭に UTF-8 BOM(U+FEFF)が付く', () => {
		const csv = buildErrorRowsCsv([{ lineNumber: 2, message: 'name は必須です。' }]);
		expect(csv.charCodeAt(0)).toBe(0xfeff);
	});

	it('ヘッダ行が lineNumber/message + TAG_CSV_COLUMNS の順になる', () => {
		const csv = buildErrorRowsCsv([]);
		const [header] = parseCsv(stripBom(csv));
		expect(header).toEqual(['lineNumber', 'message', ...TAG_CSV_COLUMNS]);
	});

	it('0件でもヘッダ行だけの CSV になる', () => {
		const csv = buildErrorRowsCsv([]);
		const table = parseCsv(stripBom(csv));
		expect(table).toHaveLength(1);
	});

	it('lineNumber/message がそのまま出力される', () => {
		const csv = buildErrorRowsCsv([
			{ lineNumber: 2, message: 'name は必須です。' },
			{ lineNumber: 5, message: 'dataType "xyz" は不正な値です。' }
		]);
		const table = parseCsv(stripBom(csv));
		expect(table[1][0]).toBe('2');
		expect(table[1][1]).toBe('name は必須です。');
		expect(table[2][0]).toBe('5');
		expect(table[2][1]).toBe('dataType "xyz" は不正な値です。');
	});

	it('original が渡されればそのまま TAG_CSV_COLUMNS の列順で後続列に出る', () => {
		const original = row({ name: '' });
		const csv = buildErrorRowsCsv([{ lineNumber: 2, message: 'name は必須です。', original }]);
		const table = parseCsv(stripBom(csv));
		expect(table[1].slice(2)).toEqual(original);
	});

	it('original が無い行は元データ列が全て空欄になる', () => {
		const csv = buildErrorRowsCsv([{ lineNumber: 2, message: 'name は必須です。' }]);
		const table = parseCsv(stripBom(csv));
		expect(table[1].slice(2)).toEqual(TAG_CSV_COLUMNS.map(() => ''));
	});

	it('original が TAG_CSV_COLUMNS より短い場合、足りない列は空欄になる', () => {
		const csv = buildErrorRowsCsv([
			{ lineNumber: 2, message: 'エラー', original: ['ConnA', 'GroupX'] }
		]);
		const table = parseCsv(stripBom(csv));
		expect(table[1][2]).toBe('ConnA');
		expect(table[1][3]).toBe('GroupX');
		expect(table[1][4]).toBe('');
	});

	it('複数行のエラーをまとめて1つの CSV にできる(再ダウンロード想定)', () => {
		const csv = buildErrorRowsCsv([
			{ lineNumber: 2, message: 'エラーA', original: row({ name: '' }) },
			{ lineNumber: 3, message: 'エラーB', original: row({ dataType: 'xyz' }) }
		]);
		const table = parseCsv(stripBom(csv));
		expect(table).toHaveLength(3);
	});
});

// ==============================================================================
// checkCsvSizeLimit / checkCsvRowLimit (T18-3d)
// ==============================================================================

describe('checkCsvSizeLimit', () => {
	it('上限ちょうどは OK', () => {
		expect(checkCsvSizeLimit(MAX_CSV_BYTES)).toEqual({ ok: true });
	});

	it('上限未満は OK', () => {
		expect(checkCsvSizeLimit(MAX_CSV_BYTES - 1)).toEqual({ ok: true });
	});

	it('上限超過は NG(上限値を含む日本語メッセージ)', () => {
		const result = checkCsvSizeLimit(MAX_CSV_BYTES + 1);
		expect(result.ok).toBe(false);
		if (result.ok) throw new Error('unreachable');
		expect(result.message).toContain('5MB');
	});

	it('0バイトは OK', () => {
		expect(checkCsvSizeLimit(0)).toEqual({ ok: true });
	});
});

describe('checkCsvRowLimit', () => {
	it('上限ちょうどは OK', () => {
		expect(checkCsvRowLimit(MAX_CSV_ROWS)).toEqual({ ok: true });
	});

	it('上限未満は OK', () => {
		expect(checkCsvRowLimit(MAX_CSV_ROWS - 1)).toEqual({ ok: true });
	});

	it('上限超過は NG(上限値を含む日本語メッセージ)', () => {
		const result = checkCsvRowLimit(MAX_CSV_ROWS + 1);
		expect(result.ok).toBe(false);
		if (result.ok) throw new Error('unreachable');
		expect(result.message).toContain('10,000');
	});

	it('0件は OK', () => {
		expect(checkCsvRowLimit(0)).toEqual({ ok: true });
	});
});
