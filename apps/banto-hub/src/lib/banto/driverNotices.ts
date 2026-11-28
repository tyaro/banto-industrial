/**
 * ドライバごとの注意書き（接続の画面に出す）。設定欄は `GET /api/drivers` のスキーマから
 * 描く汎用の部品なので、プロトコル固有の注意はここで持つ
 * （docs/chronogazer/opcua-client-design.md §4.1）。
 */

/** `banto_driver_opcua::PROTOCOL`（OPC UA のクライアント）と同じ値。 */
export const OPCUA_PROTOCOL = 'opcua';

/** OPC UA は暗号化なし（SecurityPolicy None）・匿名でつなぐ。 */
export const OPCUA_PLAINTEXT_NOTICE =
	'OPC UA は暗号化なし（SecurityPolicy None）・匿名でつなぎます。値は平文で流れ、相手のサーバーが本物かも確かめません。閉じた制御 LAN の中で使ってください。';

/** `protocol` の接続に出す注意書き。無ければ `null`。 */
export function driverNotice(protocol: string): string | null {
	return protocol === OPCUA_PROTOCOL ? OPCUA_PLAINTEXT_NOTICE : null;
}
