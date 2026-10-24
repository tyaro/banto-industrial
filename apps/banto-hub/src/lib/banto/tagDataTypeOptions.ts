/**
 * R2b-6d-2b（docs/banto-hub-driver-ui-design.md §7）: タグのデータ型プルダウンの選択肢を、
 * 選択中の接続から決める依存の少ない純関数モジュール（`tags/+page.svelte` の単票フォーム・
 * 連続登録・構造体登録の 3 か所が使う）。
 *
 * - **ドライバの接続**（`GET /api/drivers` に載る `slmp`・`modbus-tcp` など）: ドライバの
 *   `supportedKinds` にある型だけ（順序は {@link DATA_TYPE_OPTIONS} の順）。
 * - **それ以外**（ホストの接続 `push`・`postgres`・`virtual`、接続の無いグループ
 *   `computed`・`internal`・`db`・未選択、まだ一覧が届いていない・一覧に無いドライバ）:
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
	driver?: DriverInfo
): DataTypeOption[] {
	if (driver) {
		const kinds = new Set(driver.supportedKinds);
		return DATA_TYPE_OPTIONS.filter((opt) => kinds.has(opt.value));
	}
	// R2a-2e（docs/banto-hub-ingest-design.md §3.3）: push のタグは数値（64bit を
	// 含む）と bit だけ。string は送れない（サーバーの `push_tag_errors` が断る）。
	if (protocol === 'push') return DATA_TYPE_OPTIONS.filter((opt) => opt.value !== 'string');
	return DATA_TYPE_OPTIONS.filter((opt) => !SIXTY_FOUR_BIT_TYPES.includes(opt.value));
}
