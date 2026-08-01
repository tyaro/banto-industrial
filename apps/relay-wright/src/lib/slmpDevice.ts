/**
 * SLMP（MELSEC）デバイス記法のクライアント側ミラー。タグ登録画面の
 * 「連続登録」がプレビュー用にアドレス列を生成するためだけに使う。
 *
 * **正本は `crates/banto-plc/src/slmp/address.rs`。** 本ファイルは同 Rust
 * ファイルの DEVICE_TABLE から「ニーモニック・基数（radix）・ビット/ワード
 * 種別（access）」のみを写した最小サブセットで、それ以外（ワイヤコード等）
 * は持たない。デバイスの追加・変更は必ず Rust 側に合わせて更新すること。
 * 登録の正否は従来どおりサーバー側（Rust パーサー＋プランナー）が最終判定
 * するため、ここが古くても誤登録には至らない（プレビューがずれるだけ）。
 */

export interface SlmpDeviceInfo {
	/** MELSEC 資料どおりの表記（大文字）。 */
	mnemonic: string;
	/** デバイス番号の表記基数。X/Y/B/W/SB/SW/DX/DY は 16、それ以外は 10。 */
	radix: 10 | 16;
	/** ビットデバイスかワードデバイスか（データ型との整合検証に使う）。 */
	access: 'bit' | 'word';
}

/**
 * address.rs の DEVICE_TABLE と同順（2文字ニーモニック → 1文字ニーモニック）。
 * パーサーは先頭一致で最初にマッチしたものを採用するため、この
 * 「長いニーモニック優先」順が曖昧さ回避のすべて（`SD100` を `S`+`D100` と
 * 誤読しない）— Rust 側と同じ理屈をそのまま写している。
 */
export const SLMP_DEVICE_TABLE: readonly SlmpDeviceInfo[] = [
	// 2文字ニーモニック。
	{ mnemonic: 'ZR', radix: 10, access: 'word' },
	{ mnemonic: 'TS', radix: 10, access: 'bit' },
	{ mnemonic: 'TC', radix: 10, access: 'bit' },
	{ mnemonic: 'TN', radix: 10, access: 'word' },
	{ mnemonic: 'SS', radix: 10, access: 'bit' },
	{ mnemonic: 'SC', radix: 10, access: 'bit' },
	{ mnemonic: 'SN', radix: 10, access: 'word' },
	{ mnemonic: 'CS', radix: 10, access: 'bit' },
	{ mnemonic: 'CC', radix: 10, access: 'bit' },
	{ mnemonic: 'CN', radix: 10, access: 'word' },
	{ mnemonic: 'SB', radix: 16, access: 'bit' },
	{ mnemonic: 'SD', radix: 10, access: 'word' },
	{ mnemonic: 'SM', radix: 10, access: 'bit' },
	{ mnemonic: 'SW', radix: 16, access: 'word' },
	{ mnemonic: 'DX', radix: 16, access: 'bit' },
	{ mnemonic: 'DY', radix: 16, access: 'bit' },
	// 1文字ニーモニック。
	{ mnemonic: 'X', radix: 16, access: 'bit' },
	{ mnemonic: 'Y', radix: 16, access: 'bit' },
	{ mnemonic: 'M', radix: 10, access: 'bit' },
	{ mnemonic: 'L', radix: 10, access: 'bit' },
	{ mnemonic: 'F', radix: 10, access: 'bit' },
	{ mnemonic: 'V', radix: 10, access: 'bit' },
	{ mnemonic: 'B', radix: 16, access: 'bit' },
	{ mnemonic: 'D', radix: 10, access: 'word' },
	{ mnemonic: 'W', radix: 16, access: 'word' },
	{ mnemonic: 'S', radix: 10, access: 'bit' },
	{ mnemonic: 'Z', radix: 10, access: 'word' },
	{ mnemonic: 'R', radix: 10, access: 'word' }
];

/**
 * SLMP のデバイス指定フィールドが番号を3バイトで運ぶことによるワイヤ上限
 * （address.rs の MAX_DEVICE_NUMBER と同値）。これを超える番号は Rust 側
 * パーサーも拒否する。
 */
export const SLMP_MAX_DEVICE_NUMBER = 0x00ff_ffff;

const DECIMAL_DIGITS = /^[0-9]+$/;
const HEX_DIGITS = /^[0-9A-F]+$/;

/**
 * `"D100"` / `"x1a"` 形式を `(デバイス, 番号)` に解釈する。Rust 側 `parse`
 * と同じ受理規則: 前後空白トリム・大文字小文字不問・長いニーモニック優先
 * の先頭一致・番号はデバイス固有の基数で全桁有効・上限
 * [`SLMP_MAX_DEVICE_NUMBER`] 以内。解釈できなければ null（呼び出し側で
 * インラインエラーにする）。
 */
export function parseSlmpDevice(raw: string): { device: SlmpDeviceInfo; number: number } | null {
	const upper = raw.trim().toUpperCase();
	const device = SLMP_DEVICE_TABLE.find((d) => upper.startsWith(d.mnemonic));
	if (!device) return null;
	const digits = upper.slice(device.mnemonic.length);
	if (digits === '') return null;
	if (!(device.radix === 16 ? HEX_DIGITS : DECIMAL_DIGITS).test(digits)) return null;
	const number = parseInt(digits, device.radix);
	if (!Number.isSafeInteger(number) || number > SLMP_MAX_DEVICE_NUMBER) return null;
	return { device, number };
}

/**
 * `(デバイス, 番号)` を Rust 側 `format` と同じ綴りに戻す（16進デバイスは
 * 大文字16進・それ以外は10進）。
 */
export function formatSlmpDevice(device: SlmpDeviceInfo, number: number): string {
	const digits = device.radix === 16 ? number.toString(16).toUpperCase() : String(number);
	return `${device.mnemonic}${digits}`;
}
