/**
 * R2b-6d-2b（docs/banto-hub-driver-ui-design.md §7）: タグのデータ型プルダウンの選択肢を、
 * 選択中の接続から決める依存の少ない純関数モジュール（`tags/+page.svelte` の単票フォーム・
 * 連続登録・構造体登録の 3 か所が使う）。
 *
 * - **ドライバの接続**（`GET /api/drivers` に載る `slmp`・`modbus-tcp` など）: ドライバの
 *   `supportedKinds` にある型だけ（順序は {@link DATA_TYPE_OPTIONS} の順）。
 * - **ドライバの一覧がまだ届いていない**（`driversLoaded === false`）のドライバの id:
 *   全部の型（{@link DATA_TYPE_OPTIONS}）。編集中の既存タグの型（i64 など）が選択肢から
 *   消えないようにするため。最終の検証はサーバー。
 * - **それ以外**（ホストの接続 `push`・`postgres`・`virtual`、接続の無いグループ
 *   `computed`・`internal`・`db`・未選択、一覧は届いたが載っていないドライバ）:
 *   ホストの表。`push` は string を除く全部（64bit を含む）、それ以外は 64bit を除く
 *   安全側（サーバー側 `banto-tags` が最終防衛線として拒否する）。
 */
import type { DriverInfo, PlcProtocol, TagDataType } from './tagRegistryAdmin';

export interface DataTypeOption {
	value: TagDataType;
	label: string;
}

/**
 * 64bit 型。`tagRegistryAdmin.ts` の `MODBUS_ONLY_DATA_TYPES`（サーバー側の定数の写し）と同じ
 * 中身。あちらは import すると HTTP クライアントの副作用を引くので、依存ゼロを保つためここに持つ。
 */
const SIXTY_FOUR_BIT_TYPES: readonly TagDataType[] = ['i64', 'u64', 'f64'];

/** ホストが自前で扱う接続のプロトコル（`/api/drivers` に載らない）。 */
const HOST_PROTOCOLS: readonly string[] = ['push', 'postgres', 'virtual'];

/**
 * 一覧が未着（`driversLoaded === false`）で、protocol がホストの接続でも未選択でもない
 * （= ドライバの id かもしれない）とき true。`tagAddressHelp.ts` も使う。
 */
export function isDriverPending(
	protocol: PlcProtocol | undefined,
	driversLoaded: boolean,
	driver?: DriverInfo
): boolean {
	return (
		!driversLoaded &&
		driver === undefined &&
		protocol !== undefined &&
		!HOST_PROTOCOLS.includes(protocol)
	);
}

/** 全データ型（表示順）。 */
export const DATA_TYPE_OPTIONS: DataTypeOption[] = [
	{ value: 'bit', label: 'bit（真偽値1点）' },
	{ value: 'i16', label: 'i16（符号あり16bit）' },
	{ value: 'u16', label: 'u16（符号なし16bit）' },
	{ value: 'i32', label: 'i32（符号あり32bit）' },
	{ value: 'u32', label: 'u32（符号なし32bit）' },
	{ value: 'f32', label: 'f32（浮動小数点32bit）' },
	{ value: 'i64', label: 'i64（符号あり64bit）' },
	{ value: 'u64', label: 'u64（符号なし64bit）' },
	{ value: 'f64', label: 'f64（浮動小数点64bit・倍精度）' },
	{ value: 'string', label: 'string（文字列）' }
];

/**
 * 選択中の接続のプロトコルとそのドライバ（`/api/drivers` から `id === protocol` で引いたもの、
 * 無ければ `undefined`）から、選べるデータ型を返す。
 */
export function dataTypeOptionsFor(
	protocol: PlcProtocol | undefined,
	driver?: DriverInfo,
	driversLoaded = true
): DataTypeOption[] {
	if (driver) {
		const kinds = new Set(driver.supportedKinds);
		return DATA_TYPE_OPTIONS.filter((opt) => kinds.has(opt.value));
	}
	if (isDriverPending(protocol, driversLoaded, driver)) return DATA_TYPE_OPTIONS;
	// R2a-2e（docs/banto-hub-ingest-design.md §3.3）: push のタグは数値（64bit を
	// 含む）と bit だけ。string は送れない（サーバーの `push_tag_errors` が断る）。
	if (protocol === 'push') return DATA_TYPE_OPTIONS.filter((opt) => opt.value !== 'string');
	return DATA_TYPE_OPTIONS.filter((opt) => !SIXTY_FOUR_BIT_TYPES.includes(opt.value));
}
