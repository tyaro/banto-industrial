/**
 * T18-2b（docs/banto-hub-t18-design.md「T18-2b プロトコル別アドレス補助」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-6 / TAG-UX-B）: `tags/+page.svelte`
 * の単票 create/edit フォーム（`tagFields` スニペット）のアドレス欄が、
 * 選択中の接続プロトコル（`slmp` / `modbus-tcp` / `virtual`）に応じて
 * 「アドレス例」「対応デバイス／占有範囲」「bit 指定可否」を切り替えるための
 * 依存ゼロの純関数・定数モジュール。`tagFormLayout.ts` と同じ方針 -
 * Svelte 側は `connectionForGroupId(form.collectionGroupId)?.protocol` と
 * `form.dataType` を渡すだけの薄いラッパーに留め、実際のマッピングは
 * ここでテストする。
 *
 * アドレス記法そのものの正は `crates/banto-plc/src/address.rs`
 * （Modbus 参照番号 `Address::parse`）と `crates/banto-plc/src/slmp/address.rs`
 * （MELSEC デバイス記法・`SlmpDevice::access`/`radix`）。本モジュールは
 * その2つのパーサが実際に受理する規則を UI 向けの日本語ヒントへ要約した
 * ものであり、検証そのものはしない（実際の検証はサーバー側 preflight /
 * `createTagsBatch(..., dryRun=true)` に委ねる - このモジュールは
 * 「何を書けばよいか」を示すだけ）。
 *
 * 受け入れ条件（TAG-UX-B）「Modbus 選択時に `D100` を推奨例として表示
 * しない」を満たすため、`examples` はプロトコルごとに完全に独立したリスト
 * を持つ（共有・フォールバックしない）。
 */
import type { PlcProtocol, TagDataType } from './tagRegistryAdmin';

/** アドレス例1件。`address` は入力欄にそのまま書ける値、`description` は短い注記。 */
export interface AddressExample {
	address: string;
	description: string;
}

/** プロトコル別のアドレス入力補助一式。`tagFields` のアドレス欄がそのまま表示に使う。 */
export interface AddressHelp {
	/** `<input>` の `placeholder`。 */
	placeholder: string;
	/** 「アドレス例」として列挙する候補。 */
	examples: AddressExample[];
	/** 対応デバイス／エリアの説明。 */
	deviceHint: string;
	/** 選択中の `dataType` を踏まえた占有範囲（word数）の説明。 */
	occupancyHint: string;
	/** bit 指定（`.N`）の可否・書式の説明。 */
	bitHint: string;
}

/**
 * `data_type` の占有 word 数区分。`crates/banto-plc/src/types.rs`
 * `DataType::register_span`（`bit`/`i16`/`u16` = 1、`i32`/`u32`/`f32` = 2）と
 * `TagInput.stringLength`（`dataType === 'string'` のときのみ意味を持つ）を
 * まとめて3区分に落とす - ここではワード数の正確な計算式ではなく、
 * 「1点」「2点連続」「指定した word 数だけ連続」のどれかだけを言えればよい。
 */
type OccupancyKind = 'bit' | 'single-word' | 'double-word' | 'string';

function occupancyKindOf(dataType: TagDataType): OccupancyKind {
	if (dataType === 'bit') return 'bit';
	if (dataType === 'string') return 'string';
	if (dataType === 'i32' || dataType === 'u32' || dataType === 'f32') return 'double-word';
	return 'single-word';
}

const SLMP_EXAMPLES: AddressExample[] = [
	{ address: 'D100', description: 'データレジスタ（ワード）' },
	{ address: 'M50', description: '内部リレー（ビット）' },
	{ address: 'X1A', description: '入力リレー（ビット・16進）' },
	{ address: 'D100.5', description: 'D100 の 5 ビット目（ビット指定）' }
];

const MODBUS_EXAMPLES: AddressExample[] = [
	{ address: '40001', description: '保持レジスタ（ワード・書き込み可）' },
	{ address: '30001', description: '入力レジスタ（ワード・読み取り専用）' },
	{ address: '00001', description: 'コイル（ビット・書き込み可）' },
	{ address: '10001', description: 'ディスクリート入力（ビット・読み取り専用）' }
];

const SLMP_DEVICE_HINT =
	'ビットデバイス（X/Y/M/L/F/V/B/S/TS/TC/SS/SC/CS/CC/SB/SM/DX/DY）とワードデバイス（D/W/Z/R/ZR/TN/SN/CN/SD/SW）があります。' +
	'X/Y/B/W/SB/SW/DX/DY は16進数、それ以外のデバイス番号は10進数で書きます。';

const MODBUS_DEVICE_HINT =
	'先頭の桁がエリアを表します: 0xxxx=コイル（ビット）、1xxxx=ディスクリート入力（ビット・読み取り専用）、' +
	'3xxxx=入力レジスタ（ワード・読み取り専用）、4xxxx=保持レジスタ（ワード）。通常は5桁、9999を超える番号は' +
	'6桁表記（400001〜）を使います。';

const VIRTUAL_DEVICE_HINT = '内部（mem）／演算（calc）タグは PLC アドレスを持ちません。';

function slmpOccupancyHint(kind: OccupancyKind): string {
	switch (kind) {
		case 'bit':
			return 'bit 型はビットデバイスのアドレスをそのまま1点指定します（例: M50）。';
		case 'double-word':
			return '32bit 型（i32/u32/f32）はワードデバイス2点分を連続して占有します（例: D100 なら D100・D101）。';
		case 'string':
			return 'string 型は「文字列長（word数）」で指定した word 数を先頭アドレスから連続して占有します。';
		case 'single-word':
		default:
			return '16bit 型（i16/u16）はワードデバイス1点分を占有します。';
	}
}

function modbusOccupancyHint(kind: OccupancyKind): string {
	switch (kind) {
		case 'bit':
			return 'bit 型はコイル（0xxxx）またはディスクリート入力（1xxxx）を1点指定します。';
		case 'double-word':
			return '32bit 型（i32/u32/f32）は入力レジスタ／保持レジスタ2点分を連続して占有します（例: 40001 なら 40001・40002）。';
		case 'string':
			return 'string 型は「文字列長（word数）」で指定した word 数（入力／保持レジスタ）を先頭アドレスから連続して占有します。';
		case 'single-word':
		default:
			return '16bit 型（i16/u16）は入力レジスタ／保持レジスタ1点分を占有します。';
	}
}

const SLMP_BIT_HINT =
	'ワードデバイスの特定ビットだけを読み書きするときは「D100.5」のように「.」+ビット位置（0〜15、10進）を付けます' +
	'（data_type = bit のタグでのみ使えます）。ビットデバイス自体には「.」指定はできません。';

const MODBUS_BIT_HINT =
	'保持レジスタ／入力レジスタの特定ビットだけを読み書きするときは「40001.3」のように「.」+ビット位置（0〜15）を付けます' +
	'（data_type = bit のタグでのみ使えます）。コイル／ディスクリート入力は既にビット単位なので「.」指定はできません。';

const VIRTUAL_BIT_HINT = '該当なし（PLC アドレスを使わないため）。';

/**
 * 接続が未選択（`protocol === undefined`）のときの汎用ヒント。TAG-UX-B の
 * 入力順（種別→接続／グループ→名前→アドレス→型）どおりならアドレス欄に
 * 到達する時点でプロトコルは確定しているはずだが、フォームは自由な順で
 * 触れるため防御的に用意する - Modbus を「選択」したわけではないので
 * `D100` を含めても受け入れ条件には抵触しない。
 */
function unknownProtocolHelp(_dataType: TagDataType): AddressHelp {
	return {
		placeholder: '例: D100 / 40001',
		examples: [],
		deviceHint:
			'収集グループ（接続）を選択すると、そのプロトコルに合ったデバイス一覧を表示します。',
		occupancyHint: '',
		bitHint: '接続を選択すると、bit 指定（.N）が使えるかどうかを表示します。'
	};
}

/**
 * 選択中のプロトコルと `data_type` から、アドレス欄に出すヒント一式を
 * 組み立てる。`protocol` は `connectionForGroupId(form.collectionGroupId)
 * ?.protocol`（グループ未選択なら `undefined`）をそのまま渡す想定。
 */
export function addressHelpFor(
	protocol: PlcProtocol | undefined,
	dataType: TagDataType
): AddressHelp {
	const kind = occupancyKindOf(dataType);
	switch (protocol) {
		case 'slmp':
			return {
				placeholder: 'D100（ビット指定: D100.5）',
				examples: SLMP_EXAMPLES,
				deviceHint: SLMP_DEVICE_HINT,
				occupancyHint: slmpOccupancyHint(kind),
				bitHint: SLMP_BIT_HINT
			};
		case 'modbus-tcp':
			return {
				placeholder: '40001（ビット指定: 40001.3）',
				examples: MODBUS_EXAMPLES,
				deviceHint: MODBUS_DEVICE_HINT,
				occupancyHint: modbusOccupancyHint(kind),
				bitHint: MODBUS_BIT_HINT
			};
		case 'virtual':
			return {
				placeholder: '',
				examples: [],
				deviceHint: VIRTUAL_DEVICE_HINT,
				occupancyHint: '',
				bitHint: VIRTUAL_BIT_HINT
			};
		default:
			return unknownProtocolHelp(dataType);
	}
}
