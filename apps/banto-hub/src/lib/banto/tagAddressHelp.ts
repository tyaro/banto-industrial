/**
 * T18-2b（docs/banto-hub-t18-design.md「T18-2b プロトコル別アドレス補助」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-6 / TAG-UX-B）、R2b-6d-2b
 * （docs/banto-hub-driver-ui-design.md §7）: `tags/+page.svelte` の単票 create/edit
 * フォーム（`tagFields` スニペット）のアドレス欄の補助を作る、依存ゼロの純関数モジュール。
 * `tagFormLayout.ts` と同じ方針 - Svelte 側は選択中の接続のプロトコルと、
 * `GET /api/drivers` の該当ドライバ（{@link DriverInfo}）と `form.dataType` を渡すだけの
 * 薄いラッパーに留め、マッピングはここでテストする。
 *
 * - **ドライバの接続**（`slmp`・`modbus-tcp` など `/api/drivers` に載るもの）: アドレスの
 *   文法の説明は `driver.addressHelp` をそのまま使う（中身は解析しない。TS にプロトコルの
 *   知識を持たない）。占有の注意だけは、プロトコルに依らずデータ型から作る汎用の 1 関数。
 * - **ホストの接続**（`push`・`virtual`）と未選択: `/api/drivers` に載らないので、この
 *   モジュールの表を残す。ドライバの id なのに一覧がまだ届いていない・一覧に無いときは
 *   未選択と同じ説明にする。
 *
 * 検証そのものはしない（実際の検証はサーバー側 preflight / `createTagsBatch(..., dryRun=true)`
 * に委ねる - このモジュールは「何を書けばよいか」を示すだけ）。
 */
import type { DriverInfo, PlcProtocol, TagDataType } from './tagRegistryAdmin';

/** アドレス例1件。`address` は入力欄にそのまま書ける値、`description` は短い注記。 */
export interface AddressExample {
	address: string;
	description: string;
}

/** プロトコル別のアドレス入力補助一式。`tagFields` のアドレス欄がそのまま表示に使う。 */
export interface AddressHelp {
	/**
	 * ドライバの `addressHelp`（日本語・複数行。例・デバイス・ビット指定を含む）。ドライバの
	 * 接続のときだけ非空で、画面は改行を保って表示する。このとき `examples`・`deviceHint`・
	 * `bitHint` は空。
	 */
	driverHelp: string;
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
 * `DataType::register_span`（`bit`/`i16`/`u16` = 1、`i32`/`u32`/`f32` = 2、
 * `i64`/`u64`/`f64` = 4 - #325、2026-09-08 オーナー決定）と
 * `TagInput.stringLength`（`dataType === 'string'` のときのみ意味を持つ）を
 * まとめて4区分に落とす - ここではワード数の正確な計算式ではなく、
 * 「1点」「2点連続」「4点連続」「指定した word 数だけ連続」のどれかだけを
 * 言えればよい。
 */
type OccupancyKind = 'bit' | 'single-word' | 'double-word' | 'quad-word' | 'string';

function occupancyKindOf(dataType: TagDataType): OccupancyKind {
	if (dataType === 'bit') return 'bit';
	if (dataType === 'string') return 'string';
	if (dataType === 'i32' || dataType === 'u32' || dataType === 'f32') return 'double-word';
	if (dataType === 'i64' || dataType === 'u64' || dataType === 'f64') return 'quad-word';
	return 'single-word';
}

const VIRTUAL_DEVICE_HINT = '内部（mem）／演算（calc）タグは PLC アドレスを持ちません。';

/**
 * ドライバの接続の占有の注意。プロトコルに依らず、データ型だけから作る
 * （bit = 1 点、16bit = 1 ワード、32bit = 2 ワード連続、64bit = 4 ワード連続、
 * string = 文字列長と文字コードで決まるワード数を先頭から連続）。
 */
function driverOccupancyHint(kind: OccupancyKind): string {
	switch (kind) {
		case 'bit':
			return 'bit 型はビットデバイス（コイル等）の 1 点、またはワード内の 1 ビットを指定します。';
		case 'double-word':
			return '32bit 型（i32/u32/f32）はワード 2 点分を連続して占有します（先頭アドレスの次のワードまで）。';
		case 'quad-word':
			return '64bit 型（i64/u64/f64）はワード 4 点分を連続して占有します（先頭アドレスから 4 ワード）。';
		case 'string':
			return 'string 型は「文字列長」と文字コードで決まる word 数を先頭アドレスから連続して占有します。';
		case 'single-word':
		default:
			return '16bit 型（i16/u16）はワード 1 点分を占有します。';
	}
}

const VIRTUAL_BIT_HINT = '該当なし（PLC アドレスを使わないため）。';

/**
 * R2a-2e（docs/banto-hub-ingest-design.md §3.3）: `push` 接続のタグの `address` は
 * PLC アドレスではなく、**送り手が決めるキー**（送り手が値に付けて送る文字列）。
 * 規則は `banto_tags::validate_push_key`（前後の空白を除いて保存、1〜256 文字、
 * 制御文字なし、大文字小文字を区別）と、`push_tag_errors`（同じ接続の中で一意、
 * `string` 不可、書き込み不可、スケーリングなし）。
 */
const PUSH_EXAMPLES: AddressExample[] = [
	{ address: 'line1.temp', description: '送り手が付けるキー（例: DB ブリッジの列の対応）' },
	{ address: 'Line1/Temp', description: '大文字小文字を区別します' }
];

const PUSH_DEVICE_HINT =
	'push 接続のタグのアドレスは、送り手（DB ブリッジ・スクリプト）が値に付けて送るキーです' +
	'（1〜256 文字、前後の空白は除いて保存、大文字小文字を区別）。同じ接続の中では一意にしてください。' +
	'送り手が登録の無いキーで送った値は受け付けられず、状態の「未知のキー」に出ます。';

function pushOccupancyHint(kind: OccupancyKind): string {
	return kind === 'string'
		? 'push のタグに string 型は使えません（数値と bit だけ）。'
		: '送り手は工学値をそのまま送ります（スケーリングは使えません）。書き込みはできません。';
}

const PUSH_BIT_HINT =
	'該当なし（ビット指定はありません）。bit 型のタグには 0 / 1 / true / false を送ります。';

/**
 * 接続が未選択（`protocol === undefined`）のときの汎用ヒント。TAG-UX-B の
 * 入力順（種別→接続／グループ→名前→アドレス→型）どおりならアドレス欄に
 * 到達する時点でプロトコルは確定しているはずだが、フォームは自由な順で
 * 触れるため防御的に用意する - Modbus を「選択」したわけではないので
 * `D100` を含めても受け入れ条件には抵触しない。
 */
function unknownProtocolHelp(_dataType: TagDataType): AddressHelp {
	return {
		driverHelp: '',
		placeholder: '例: D100 / 40001',
		examples: [],
		deviceHint:
			'収集グループ（接続）を選択すると、そのプロトコルに合ったデバイス一覧を表示します。',
		occupancyHint: '',
		bitHint: '接続を選択すると、bit 指定（.N）が使えるかどうかを表示します。'
	};
}

/**
 * 選択中のプロトコル・そのドライバ・`data_type` から、アドレス欄に出すヒント一式を組み立てる。
 * `protocol` は `connectionForGroupId(form.collectionGroupId)?.protocol`（グループ未選択なら
 * `undefined`）、`driver` は `/api/drivers` の一覧から `id === protocol` で引いたもの
 * （無ければ `undefined`）をそのまま渡す想定。`driver` があればホストの表より優先する。
 */
export function addressHelpFor(
	protocol: PlcProtocol | undefined,
	dataType: TagDataType,
	driver?: DriverInfo
): AddressHelp {
	const kind = occupancyKindOf(dataType);
	if (driver) {
		return {
			driverHelp: driver.addressHelp,
			placeholder: '',
			examples: [],
			deviceHint: '',
			occupancyHint: driverOccupancyHint(kind),
			bitHint: ''
		};
	}
	switch (protocol) {
		case 'push':
			return {
				driverHelp: '',
				placeholder: 'line1.temp（送り手が付けるキー）',
				examples: PUSH_EXAMPLES,
				deviceHint: PUSH_DEVICE_HINT,
				occupancyHint: pushOccupancyHint(kind),
				bitHint: PUSH_BIT_HINT
			};
		case 'virtual':
			return {
				driverHelp: '',
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
