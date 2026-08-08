/**
 * T11-2 CSV インポート/エクスポート（docs/ux-plan.md §3「T11: タグの一括
 * 登録（連続登録 + CSV インポート/エクスポート）」）: タグ定義を CSV で
 * バックアップ/複製/レビューする。T11-1 の一括登録 API
 * （`POST /api/tags/batch`、`$lib/banto/tagRegistryAdmin.ts::createTagsBatch`）
 * をそのまま消費する第2の呼び出し元 — サーバー側の変更はない
 * （`apps/banto-hub/core/src/rest.rs::tags_batch` のコメントに「CSV
 * インポート(T11-2)ではこの index がそのまま CSV の行番号(ヘッダ行を
 * 除く0起点データ行)に対応する想定」と明記されている）。
 *
 * **依存なしの自前 RFC4180 実装**: `apps/banto-hub/package.json`・ルート
 * `package.json` を確認したが CSV パースライブラリは存在せず、この
 * 機能のためだけに新規依存を追加しない方針（実装指示）。RFC4180 は
 * 「引用符付きフィールド」「フィールド内の埋め込みカンマ/改行」
 * 「`""` によるエスケープ」の3点さえ実装すれば十分小さいため、自前実装
 * のコストは見合うと判断した。
 *
 * `continuousRegistration.ts` と同じ設計 — 依存なしの純関数群。テストは
 * 同ディレクトリの `tagCsv.test.ts`（vitest、H5 で導入）が全公開 API を
 * 網羅する。
 */
import type {
	CollectionGroup,
	PlcConnection,
	Tag,
	TagDataType,
	TagInput,
	TagKind
} from './tagRegistryAdmin';

// --- RFC4180 プリミティブ ----------------------------------------------------

/** 先頭の UTF-8 BOM（U+FEFF）があれば除去する。無くてもそのまま通す。 */
export function stripBom(text: string): string {
	return text.length > 0 && text.charCodeAt(0) === 0xfeff ? text.slice(1) : text;
}

/**
 * RFC4180 の最小実装。引用符付きフィールド（埋め込みカンマ・埋め込み
 * 改行 `\n`/`\r\n`・`""` エスケープ）、引用符無しフィールド、`\r\n`/`\n`
 * どちらの改行も受け付ける。
 *
 * **設計判断: 不正な入力でも例外を投げず、可能な範囲で読み取る**。
 * 「壊れた CSV をアップロードしたら白画面」より「読めるところまで読んで、
 * 意味のあるエラー行を出す」方がユーザーにとって親切なため。具体的には
 * 未終端の引用符（閉じる `"` が無いまま EOF に到達）の場合、以降の全文字
 * （区切り文字や改行を含む）をそのフィールドの内容としてそのまま取り込み、
 * ファイル末尾で1行として確定する（例外なし・クラッシュなし）。
 *
 * **末尾の空行は無視する**（EOF 直前の1個の改行だけでは、空フィールド1個
 * だけの余分な最終行を生成しない）。ファイル中間の空行（例:
 * `a,b\n\nc,d`）は `['']` という1セルの行として現れる —
 * {@link parseTagsCsv} 側でこれをデータ行としてではなく空行として読み
 * 飛ばす。
 */
export function parseCsv(text: string): string[][] {
	const rows: string[][] = [];
	let row: string[] = [];
	let field = '';
	let inQuotes = false;
	const len = text.length;
	let i = 0;

	while (i < len) {
		const c = text[i];
		if (inQuotes) {
			if (c === '"') {
				if (text[i + 1] === '"') {
					field += '"';
					i += 2;
				} else {
					inQuotes = false;
					i += 1;
				}
			} else {
				field += c;
				i += 1;
			}
			continue;
		}

		if (c === '"') {
			inQuotes = true;
			i += 1;
			continue;
		}
		if (c === ',') {
			row.push(field);
			field = '';
			i += 1;
			continue;
		}
		if (c === '\r' || c === '\n') {
			if (c === '\r' && text[i + 1] === '\n') i += 2;
			else i += 1;
			row.push(field);
			rows.push(row);
			row = [];
			field = '';
			continue;
		}
		field += c;
		i += 1;
	}

	// EOF: 直前の行区切りで既にフィールド/行が確定済みなら何も残っていない
	// （＝末尾の空行を余分に生成しない）。改行無しで終わるファイルや
	// 未終端の引用符で終わるファイルは、ここで最後の1行として確定する。
	if (field !== '' || row.length > 0) {
		row.push(field);
		rows.push(row);
	}
	return rows;
}

function csvFieldNeedsQuoting(field: string): boolean {
	return field.includes(',') || field.includes('"') || field.includes('\n') || field.includes('\r');
}

function quoteCsvField(field: string): string {
	if (!csvFieldNeedsQuoting(field)) return field;
	return `"${field.replace(/"/g, '""')}"`;
}

/**
 * {@link parseCsv} の逆関数。カンマ/二重引用符/改行を含むフィールドだけ
 * 引用符で囲み、内部の `"` を `""` に二重化する。行区切りは Excel との
 * 相性を優先して CRLF（`\r\n`）固定。`parseCsv(serializeCsv(rows)) ===
 * rows` が任意の入力（埋め込みカンマ・引用符・改行・日本語を含む）で
 * 成り立つことを `tagCsv.test.ts` のラウンドトリップテストで固定している。
 */
export function serializeCsv(rows: string[][]): string {
	if (rows.length === 0) return '';
	return rows.map((row) => row.map(quoteCsvField).join(',')).join('\r\n') + '\r\n';
}

// --- 列スキーマ（エクスポート/インポート共通の単一の情報源） ----------------

/**
 * CSV の列順（docs/ux-plan.md §3「接続・グループは名前で参照」の決定に
 * 従い、`TagInput.collectionGroupId` を `connection`+`group` の2列の名前
 * 参照に置き換えた以外は `TagInput` の全フィールドと1:1対応）。
 * エクスポート（{@link exportTagsCsv}）とインポート（{@link parseTagsCsv}）
 * の両方がこの配列を単一の情報源として使う — 列順や列名がずれる余地を
 * なくす。
 */
export const TAG_CSV_COLUMNS = [
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
] as const;

type TagCsvColumn = (typeof TAG_CSV_COLUMNS)[number];

function numCell(v: number | null | undefined): string {
	return v === null || v === undefined ? '' : String(v);
}

/**
 * タグ一覧を CSV テキストへ変換する純関数（Blob/DOM 操作は呼び出し元の
 * `+page.svelte` が担当 — このモジュールはブラウザ API に依存しない）。
 * Excel での文字化け対策として **UTF-8 BOM を先頭に付与**する
 * （docs/ux-plan.md §3「Excel 想定のため UTF-8 BOM 付きで出力」）。
 */
export function exportTagsCsv(
	tags: Tag[],
	connections: PlcConnection[],
	groups: CollectionGroup[]
): string {
	const groupInfo = new Map<number, { groupName: string; connectionName: string }>();
	for (const g of groups) {
		const conn = connections.find((c) => c.id === g.plcConnectionId);
		groupInfo.set(g.id, { groupName: g.name, connectionName: conn?.name ?? '' });
	}

	const rows: string[][] = [[...TAG_CSV_COLUMNS]];
	for (const t of tags) {
		const info = groupInfo.get(t.collectionGroupId);
		rows.push([
			info?.connectionName ?? '',
			info?.groupName ?? '',
			t.name,
			t.address,
			t.dataType,
			numCell(t.stringLength),
			t.unit ?? '',
			String(t.decimals),
			numCell(t.rawLo),
			numCell(t.rawHi),
			numCell(t.engLo),
			numCell(t.engHi),
			numCell(t.thresholdH),
			numCell(t.thresholdHh),
			numCell(t.thresholdL),
			numCell(t.thresholdLl),
			t.enabled ? 'true' : 'false',
			t.writable ? 'true' : 'false',
			t.tagKind,
			t.expression ?? '',
			t.retain ? 'true' : 'false'
		]);
	}
	return '\uFEFF' + serializeCsv(rows);
}

// --- インポート ---------------------------------------------------------------

export interface ParsedCsvTagRow {
	/** CSV ファイル中の行番号（1起点）。ヘッダ行=1、最初のデータ行=2。 */
	lineNumber: number;
	connectionName: string;
	groupName: string;
	tag: TagInput;
}

export interface CsvRowError {
	lineNumber: number;
	message: string;
}

export type ImportTagsCsvResult =
	{ ok: true; rows: ParsedCsvTagRow[] } | { ok: false; errors: CsvRowError[] };

const TAG_DATA_TYPES: ReadonlySet<string> = new Set<TagDataType>([
	'bit',
	'i16',
	'u16',
	'i32',
	'u32',
	'f32',
	'string'
]);

const TAG_KINDS: ReadonlySet<string> = new Set<TagKind>(['plc', 'computed', 'internal']);

/**
 * 真偽値セルのパース。空欄は `defaultValue`、大小文字区別なしの
 * `true`/`false`、および `1`/`0` を受け付ける。それ以外は不正値として
 * `undefined` を返す（呼び出し元がエラーメッセージを追加する）。
 */
function parseBooleanCell(raw: string, defaultValue: boolean): boolean | undefined {
	const v = raw.trim().toLowerCase();
	if (v === '') return defaultValue;
	if (v === 'true' || v === '1') return true;
	if (v === 'false' || v === '0') return false;
	return undefined;
}

const COLUMN_INDEX: Record<TagCsvColumn, number> = Object.fromEntries(
	TAG_CSV_COLUMNS.map((c, i) => [c, i])
) as Record<TagCsvColumn, number>;

/**
 * CSV → `TagInput[]` への変換。エラーはすべての行にわたって収集し
 * （最初の不正行で止めない — all-or-nothing 表示のため）、1件でもあれば
 * `{ok: false, errors}` を返す（部分適用はしない、docs/ux-plan.md §3の
 * 決定）。
 *
 * **接続・グループは名前で解決し、見つからなければエラー（自動作成は
 * しない）** — docs/ux-plan.md §3「接続・グループは名前で参照。存在
 * しなければエラー。自動作成はしない — 暗黙の副作用を避ける」の決定
 * どおり。グループ名は接続内でのみ一意（`+page.svelte::groupsFor` と
 * 同じ前提）なので、`group` 列は解決済みの `connection` の
 * `plcConnectionId` を持つグループの中からのみ探す。
 *
 * `address`/`expression`/`retain` の強制ブランク/既定値ルールは
 * `+page.svelte::toInput()` の `tagKind` 分岐をそのまま踏襲する
 * （computed/internal の意味論はどちらもクライアント側で先取りチェック
 * するが、正の判定源はサーバー側の `create_batch` 検証 — ここでの
 * チェックは「プレビューの時点で明らかに矛盾した入力を早期に指摘する」
 * ためのもの）。
 */
export function parseTagsCsv(
	text: string,
	connections: PlcConnection[],
	groups: CollectionGroup[]
): ImportTagsCsvResult {
	const stripped = stripBom(text);
	if (stripped.trim() === '') return { ok: true, rows: [] };

	const table = parseCsv(stripped);
	if (table.length === 0) return { ok: true, rows: [] };

	const header = table[0];
	const headerMismatch =
		header.length !== TAG_CSV_COLUMNS.length || TAG_CSV_COLUMNS.some((col, i) => header[i] !== col);
	if (headerMismatch) {
		return {
			ok: false,
			errors: [
				{
					lineNumber: 1,
					message: `ヘッダ行が想定した列と一致しません。期待する列（順序どおり）: ${TAG_CSV_COLUMNS.join(', ')}`
				}
			]
		};
	}

	const dataRows = table.slice(1);
	if (dataRows.length === 0) return { ok: true, rows: [] };

	const errors: CsvRowError[] = [];
	const rows: ParsedCsvTagRow[] = [];

	dataRows.forEach((cells, i) => {
		const lineNumber = i + 2;

		// parseCsv がファイル中間の空行を1セル(空文字)の行として返す場合が
		// あるため、データ行ではなく空行として読み飛ばす（行番号のカウント
		// 自体はズラさない — forEach の添字はそのまま使う）。
		if (cells.every((c) => c.trim() === '')) return;

		const col = (name: TagCsvColumn): string => (cells[COLUMN_INDEX[name]] ?? '').trim();
		const rowErrors: string[] = [];

		const connectionName = col('connection');
		const groupName = col('group');
		const name = col('name');
		if (connectionName === '') rowErrors.push('connection は必須です。');
		if (groupName === '') rowErrors.push('group は必須です。');
		if (name === '') rowErrors.push('name は必須です。');

		const connection =
			connectionName !== '' ? connections.find((c) => c.name === connectionName) : undefined;
		if (connectionName !== '' && !connection) {
			rowErrors.push(`接続 "${connectionName}" が見つかりません。`);
		}
		let group: CollectionGroup | undefined;
		if (connection && groupName !== '') {
			group = groups.find((g) => g.name === groupName && g.plcConnectionId === connection.id);
			if (!group) {
				rowErrors.push(`グループ "${groupName}"（接続 "${connectionName}"）が見つかりません。`);
			}
		}

		const dataTypeRaw = col('dataType');
		const dataType = dataTypeRaw as TagDataType;
		if (!TAG_DATA_TYPES.has(dataTypeRaw)) {
			rowErrors.push(`dataType "${dataTypeRaw}" は不正な値です。`);
		}

		const tagKindRaw = col('tagKind');
		let tagKind: TagKind = 'plc';
		if (tagKindRaw === '') {
			tagKind = 'plc';
		} else if (TAG_KINDS.has(tagKindRaw)) {
			tagKind = tagKindRaw as TagKind;
		} else {
			rowErrors.push(`tagKind "${tagKindRaw}" は不正な値です。`);
		}

		const addressRaw = col('address');
		let address = '';
		if (tagKind === 'plc') {
			if (addressRaw === '') rowErrors.push('address は tagKind=plc のとき必須です。');
			address = addressRaw;
		}
		// computed/internal は toInput() と同じく強制的に空文字（address は
		// 既定値 '' のまま = CSV に何が書かれていても無視する）。

		let stringLength: number | undefined;
		if (dataType === 'string') {
			const slRaw = col('stringLength');
			const n = Number(slRaw);
			if (slRaw === '' || !Number.isInteger(n) || n < 1) {
				rowErrors.push('stringLength は dataType=string のとき1以上の整数が必要です。');
			} else {
				stringLength = n;
			}
		}
		// string 以外では stringLength 列の内容は無視する（送信しない）。

		const decimalsRaw = col('decimals');
		let decimals = 0;
		if (decimalsRaw !== '') {
			const n = Number(decimalsRaw);
			if (!Number.isFinite(n)) {
				rowErrors.push('decimals は数値で指定してください。');
			} else {
				decimals = n;
			}
		}

		const numField = (name: TagCsvColumn, label: string): number | undefined => {
			const raw = col(name);
			if (raw === '') return undefined;
			const n = Number(raw);
			if (!Number.isFinite(n)) {
				rowErrors.push(`${label} は数値で指定してください。`);
				return undefined;
			}
			return n;
		};
		const rawLo = numField('rawLo', 'rawLo');
		const rawHi = numField('rawHi', 'rawHi');
		const engLo = numField('engLo', 'engLo');
		const engHi = numField('engHi', 'engHi');
		const thresholdH = numField('thresholdH', 'thresholdH');
		const thresholdHh = numField('thresholdHh', 'thresholdHh');
		const thresholdL = numField('thresholdL', 'thresholdL');
		const thresholdLl = numField('thresholdLl', 'thresholdLl');

		const unitRaw = col('unit');
		const unit = unitRaw === '' ? undefined : unitRaw;

		const boolField = (name: TagCsvColumn, label: string, defaultValue: boolean): boolean => {
			const raw = col(name);
			const parsed = parseBooleanCell(raw, defaultValue);
			if (parsed === undefined) {
				rowErrors.push(`${label} は true/false（または1/0）で指定してください。`);
				return defaultValue;
			}
			return parsed;
		};
		const enabled = boolField('enabled', 'enabled', true);
		const writableRaw = boolField('writable', 'writable', false);

		const expressionRaw = col('expression');
		let expression: string | undefined;
		if (tagKind === 'computed') {
			if (expressionRaw === '') rowErrors.push('expression は tagKind=computed のとき必須です。');
			expression = expressionRaw === '' ? undefined : expressionRaw;
		}
		// plc/internal は toInput() と同じく強制的に undefined（送信しない）。

		const retainRaw = boolField('retain', 'retain', false);
		// internal 以外は toInput() と同じく強制的に false。
		const retain = tagKind === 'internal' ? retainRaw : false;

		if (rowErrors.length > 0) {
			for (const message of rowErrors) errors.push({ lineNumber, message });
			return;
		}

		const tag: TagInput = {
			name,
			collectionGroupId: group!.id,
			address,
			dataType,
			stringLength: dataType === 'string' ? stringLength : undefined,
			rawLo,
			rawHi,
			engLo,
			engHi,
			unit,
			decimals,
			thresholdH,
			thresholdHh,
			thresholdL,
			thresholdLl,
			enabled,
			// computed は toInput() と同じく常に writable=false（値は式が決める）。
			writable: tagKind === 'computed' ? false : writableRaw,
			tagKind,
			expression,
			retain
		};
		rows.push({ lineNumber, connectionName, groupName, tag });
	});

	if (errors.length > 0) return { ok: false, errors };
	return { ok: true, rows };
}
