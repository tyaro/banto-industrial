/**
 * T18-3c（docs/banto-hub-t18-design.md「T18-3c 連続登録の基数/bit 連番」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-D）: MELSEC (SLMP) デバイス
 * 記法（`D100`・`M50`・`X1A`・`D100.5`）を機械可読に扱うための、依存ゼロの
 * 純関数・定数モジュール。
 *
 * **正本は `crates/banto-plc/src/slmp/address.rs`**（`SlmpDevice` enum・
 * `DEVICE_TABLE`・`radix()`/`access()`・`parse()`/`format()`）。本ファイルは
 * その Rust 側テーブルと解析規則をフロント（連続登録のアドレス連番、
 * `continuousRegistration.ts`）向けに書き写したもので、Rust 側が変わったら
 * 手動で追従させる（自動生成ではない）。`tagAddressHelp.ts` が同じ正本を
 * 参照して UI ヒント文言（自由文字列）を組み立てているのに対し、こちらは
 * パース／整形そのものを行う点が異なる。
 *
 * `parseSlmpAddress`/`formatSlmpAddress` は Rust の `parse`/`format` と
 * 同じ規則（大文字小文字を無視、2文字ニーモニックを1文字より先に照合する
 * 最長一致、デバイスごとの基数、bit サフィックスは**16進1桁の0〜F**かつ
 * ワードデバイスのみ）を踏襲する。ただし Rust 版が返す `PlcError` は持たず、
 * 失敗はすべて `null` として表現する（フロントの他の `*.ts` 純関数群と
 * 同じ判別共用体寄りの流儀）。
 *
 * **T20-④（2026-09-04 オーナー決定）: bit サフィックスの基数を10進→16進へ
 * 是正**。Rust 側 `crates/banto-plc/src/slmp/address.rs` の同日修正
 * （「MELSEC はビット位置を10進で書く」という誤った前提の是正 - 実際の
 * MELSEC ツールは `.0`〜`.F` の16進で書く）に、この写しも追従した。
 */

/** デバイス番号を表記する基数。X/Y/B/W/SB/SW/DX/DY が16、それ以外は10。 */
export type SlmpRadix = 10 | 16;

/** ビット単位でアクセスするデバイスか、ワード単位でアクセスするデバイスか。 */
export type SlmpAccess = 'bit' | 'word';

/** 1デバイスぶんの静的情報（`crates/banto-plc/src/slmp/address.rs::SlmpDevice` の写し）。 */
export interface SlmpDeviceInfo {
	readonly mnemonic: string;
	readonly radix: SlmpRadix;
	readonly access: SlmpAccess;
}

/**
 * 全28デバイス、2文字ニーモニックを1文字ニーモニックより先に並べた順序。
 * {@link parseSlmpAddress} はこの順で先頭一致を試すため、この順序自体が
 * 正しさの前提になる（`SD100` を `S`+`D100` と誤認しない、`DX10` を
 * `D`+`X10` と誤認しない、等）。Rust 側 `DEVICE_TABLE` と1対1で対応する。
 */
const DEVICE_TABLE: readonly SlmpDeviceInfo[] = [
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

/** テスト・呼び出し元が全デバイスを列挙するための読み取り専用ビュー。 */
export const SLMP_DEVICE_TABLE: readonly SlmpDeviceInfo[] = DEVICE_TABLE;

/**
 * `.N` bit サフィックスが取り得る最大値。MELSEC のワードは16bitなので
 * 0〜15（`crates/banto-plc/src/slmp/address.rs::MAX_BIT_POSITION` と同値）。
 * 表記そのものは16進1桁（`.0`〜`.F`、T20-④）。
 */
export const MAX_BIT_POSITION = 15;

/**
 * デバイス番号の上限。SLMP のデバイス指定フィールドは3バイト幅
 * （`crates/banto-plc/src/slmp/address.rs::MAX_DEVICE_NUMBER` と同値、
 * 0x00FFFFFF）。
 */
export const MAX_DEVICE_NUMBER = 0x00ff_ffff;

/** {@link parseSlmpAddress} の成功結果。`bit` は `.N` サフィックスがあるときだけ存在する。 */
export interface ParsedSlmpAddress {
	readonly device: SlmpDeviceInfo;
	readonly mnemonic: string;
	readonly number: number;
	readonly bit?: number;
}

/**
 * MELSEC デバイス記法をパースする。`crates/banto-plc/src/slmp/address.rs::parse`
 * と同じ規則:
 *
 * - 前後の空白は無視し、大文字小文字は区別しない。
 * - `.` があれば先に切り離す（最初の `.` で分割 — 複数 `.` を含む不正な
 *   文字列は bit 部分が1文字だけにならず自動的に弾かれる）。bit 部分は
 *   **ちょうど1桁の16進**数字（`0-9A-Fa-f`）のみ、値は 0〜{@link MAX_BIT_POSITION}
 *   （T20-④、2026-09-04 オーナー決定 - 以前の実装は10進1〜2桁だったが、
 *   これは MELSEC ツール表記についての事実誤認に基づく誤りだった）。
 * - 残りの先頭から {@link SLMP_DEVICE_TABLE} を順に前方一致で試し、最初に
 *   一致したデバイスを採用する（2文字ニーモニックが先）。
 * - デバイス名の後ろの数字列を、そのデバイスの {@link SlmpRadix} で解釈する
 *   （X/Y/B/W/SB/SW/DX/DY は16進、それ以外は10進）。基数に合わない文字が
 *   混ざっていたら失敗。
 * - 数値は {@link MAX_DEVICE_NUMBER} 以下でなければならない。
 * - bit サフィックスはワードデバイスにしか付けられない（ビットデバイスは
 *   既にビット粒度なので `.N` は無意味 — 失敗として扱う）。
 *
 * 解釈できない入力はすべて `null`（Rust 版の `Err(PlcError::InvalidAddress)`
 * に相当）。
 */
export function parseSlmpAddress(raw: string): ParsedSlmpAddress | null {
	const trimmed = raw.trim();
	if (trimmed === '') return null;
	const upper = trimmed.toUpperCase();

	let base = upper;
	let bit: number | undefined;

	const dotIndex = upper.indexOf('.');
	if (dotIndex !== -1) {
		base = upper.slice(0, dotIndex);
		const bitText = upper.slice(dotIndex + 1);
		if (bitText.length !== 1 || !/^[0-9A-F]$/.test(bitText)) {
			return null;
		}
		const parsedBit = Number.parseInt(bitText, 16);
		if (parsedBit > MAX_BIT_POSITION) return null;
		bit = parsedBit;
	}

	const device = DEVICE_TABLE.find((candidate) => base.startsWith(candidate.mnemonic));
	if (!device) return null;

	const digits = base.slice(device.mnemonic.length);
	if (digits.length === 0) return null;
	const digitPattern = device.radix === 16 ? /^[0-9A-F]+$/ : /^[0-9]+$/;
	if (!digitPattern.test(digits)) return null;

	const number = Number.parseInt(digits, device.radix);
	if (!Number.isFinite(number) || number > MAX_DEVICE_NUMBER) return null;

	if (bit !== undefined && device.access !== 'word') return null;

	return { device, mnemonic: device.mnemonic, number, bit };
}

/**
 * `(mnemonic, number, bit?)` を {@link parseSlmpAddress} が受理する記法へ
 * 戻す。`crates/banto-plc/src/slmp/address.rs::format` と同じく、16進基数の
 * デバイスは大文字16進、bit は `.b` を末尾に付ける。
 *
 * `mnemonic` は {@link SLMP_DEVICE_TABLE} に存在するニーモニック（通常は
 * {@link parseSlmpAddress} が返したものをそのまま渡す）を想定しており、
 * 未知のニーモニックを渡すと例外を投げる — 呼び出し元のプログラミング
 * ミスを表す状態であって、ユーザー入力起因の失敗ではないため。
 */
export function formatSlmpAddress(mnemonic: string, number: number, bit?: number): string {
	const upper = mnemonic.toUpperCase();
	const device = DEVICE_TABLE.find((candidate) => candidate.mnemonic === upper);
	if (!device) {
		throw new Error(`formatSlmpAddress: unknown SLMP device mnemonic "${mnemonic}"`);
	}
	const base =
		device.radix === 16
			? `${device.mnemonic}${number.toString(16).toUpperCase()}`
			: `${device.mnemonic}${number}`;
	return bit === undefined ? base : `${base}.${bit.toString(16).toUpperCase()}`;
}
