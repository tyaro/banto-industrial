/**
 * T11-1 連続登録（docs/ux-plan.md §3「T11: タグの一括登録」、連続登録は
 * オーナー発案・2026-08-06 採用）: 名前パターン + 開始アドレス + 点数 +
 * 共通設定から連続タグ（{@link TagInput} の配列）を生成する。
 *
 * **設計判断（2026-08-07）: パターン展開はクライアント側（ここ）が担う**
 * - 生成結果（プレビュー表示）がそのまま `POST /api/tags/batch` への送信
 *   内容になるので、「プレビューに出た通りに登録される」を保証できる。
 * - サーバー側の一括 API（`banto_hub_core::rest::tags_batch` /
 *   `banto_tags::TagService::create_batch`）は展開済み `TagInput[]` を
 *   受け取るだけの汎用エンドポイントのまま保てる（T11-2 の CSV インポート
 *   も同じ API を、別の展開ロジック — CSV パース — から呼ぶだけで済む）。
 *
 * 依存なしの純関数群 — このリポジトリに vitest 等のユニットテスト基盤が
 * まだ無いため（`apps/banto-hub/package.json`・ルート `package.json` の
 * `test` スクリプトを確認したが、`pnpm --recursive --if-present test` が
 * 拾う対象が存在しない）、この Rust 実装以外のテストは書けていない。
 * ロジックをこのファイルの純関数として切り出してあるのは、テスト基盤が
 * 導入され次第すぐにユニットテストを足せるようにするため（実装指示の
 * 「テスト基盤が無ければ純関数として切り出してテスト可能な形にしておき、
 * その旨報告」に対応 — 詳細は完了報告参照）。
 */
import type { TagDataType, TagInput } from './tagRegistryAdmin';

/** 1回のプレビュー/バッチで生成できる点数の上限（暴走防止の安全弁）。 */
export const MAX_CONTINUOUS_COUNT = 1000;

const TWO_WORD_DATA_TYPES: ReadonlySet<TagDataType> = new Set(['i32', 'u32', 'f32']);

/**
 * データ型からアドレス増分を決める（docs/ux-plan.md §3）: ワード系
 * （bit/i16/u16）は+1、2ワード型（i32/u32/f32）は+2、string は
 * +string_length。`crates/banto-tags/src/tag.rs::ALLOWED_DATA_TYPES` に
 * 存在するデータ型はこの3分岐で尽くされる（bit タグがビットデバイス
 * （M100 等）に置かれるかワードのビット位置（D100.5）に置かれるかは
 * アドレス書式の話であって data_type の話ではないので、増分自体は
 * bit/i16/u16 のどれでも同じ「+1」でよい — 実際にどちらの形かは
 * {@link hasBitSuffix} が別軸でガードする）。
 */
export function addressIncrement(dataType: TagDataType, stringLength?: number | null): number {
	if (dataType === 'string') return Math.max(1, stringLength ?? 1);
	if (TWO_WORD_DATA_TYPES.has(dataType)) return 2;
	return 1;
}

/**
 * ビット付きアドレス（`D100.5`・`40001.3` — `crates/banto-plc/src/address.rs`
 * ・`slmp/address.rs` の「bit-in-word notation」）かどうか。
 *
 * **設計判断（2026-08-07）: ビット付きアドレスの連続登録は v1 非対応**。
 * ワード側の連続登録（例: D100, D101, ...）とビット位置側の連番（例:
 * D100.0, D100.1, ...）のどちらを操作者が意図しているかは名前パターンや
 * 開始アドレスだけからは自明に決まらず、ビット位置が15を超えたときに
 * 次のワードへ繰り上げる（D100.15 の次を D101.0 にする）べきかどうかの
 * 規則も自明ではない。誤った繰り上げ規則を実装するより、明示的に
 * 「未対応」としてエラー表示する方が安全（判断として記録）。
 */
export function hasBitSuffix(address: string): boolean {
	return /\.\d{1,2}$/.test(address.trim());
}

/**
 * アドレス末尾の10進数字の並びをインクリメントする。`prefix`（デバイス
 * ニーモニックや空文字）はそのまま保持し、数字部分だけを
 * `base + step * index` に置き換える。元の桁数を可能な限り維持するため
 * `padStart` で0埋めするが、桁上がりで元の桁数を超える場合はそのまま
 * 自然な桁数にする（例: "D9" → "D10" は問題なく増える）。
 *
 * 末尾が10進数字で終わらないアドレス（16進表記のデバイス番号、例 "X1A"
 * ・"W1FF" — `SlmpDevice::radix` が16のデバイス群）は増分できないので
 * `null` を返す。v1 の連続登録はこの形式を非対応として報告する
 * （判断: 2026-08-07 — 16進デバイス番号の連続登録は稀な用途であり、
 * 誤った桁上がり実装のリスクを冒すよりエラー表示を優先）。
 *
 * Modbus 参照番号（`"40001"` 等）は先頭の領域選択桁も含めて丸ごと1個の
 * 数字列として扱う。1区画（0-based で最大9999点、6桁形式なら
 * さらに広い）を大きく超える連続登録で領域境界（例: 49999→50000で
 * area が3→5に変わる）を跨ぐ操作は稀な用途と判断し、v1では特別扱いしない
 * （判断: 2026-08-07）。
 */
export function incrementAddress(address: string, step: number, index: number): string | null {
	const trimmed = address.trim();
	const match = /^(\D*)(\d+)$/.exec(trimmed);
	if (!match) return null;
	const [, prefix, digits] = match;
	const base = Number.parseInt(digits, 10);
	const next = base + step * index;
	if (!Number.isFinite(next) || next < 0) return null;
	const nextDigits = String(next).padStart(digits.length, '0');
	return `${prefix}${nextDigits}`;
}

/**
 * 名前パターンを展開する。`{n}` を `n`（開始番号からの連番）で置換する。
 *
 * **設計判断（2026-08-07）: `{n}` が無いパターンはエラーではなく末尾に
 * 連番を付与する**。連続登録という機能の性質上「パターンに連番を含める」
 * ことが前提だが、`{n}` の書き忘れを入力エラーとして拒否するより、
 * 「多分こう意図しただろう」という自然な結果（`temp` → `temp1`,
 * `temp2`, ...）を返す方が UI として親切、かつ生成結果はどのみち適用前
 * プレビューで確認できるので誤りがあれば気付ける。
 */
export function expandNamePattern(pattern: string, n: number): string {
	if (pattern.includes('{n}')) return pattern.split('{n}').join(String(n));
	return `${pattern}${n}`;
}

export interface ContinuousRegistrationCommon {
	collectionGroupId: number;
	dataType: TagDataType;
	stringLength?: number | null;
	unit?: string | null;
	decimals: number;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	thresholdH?: number | null;
	thresholdHh?: number | null;
	thresholdL?: number | null;
	thresholdLl?: number | null;
	enabled: boolean;
	writable: boolean;
}

export interface ContinuousRegistrationParams extends ContinuousRegistrationCommon {
	/** `{n}` を含む名前パターン（例 `temp{n}`）。含まなければ末尾に連番付与。 */
	namePattern: string;
	/** 連番の開始値（`{n}` に入る最初の数）。 */
	startNumber: number;
	/** 1件目のアドレス。2件目以降は {@link addressIncrement} 分ずつ増える。 */
	startAddress: string;
	/** 生成する点数（1以上、{@link MAX_CONTINUOUS_COUNT} 以下）。 */
	count: number;
}

export interface ContinuousRegistrationRow {
	name: string;
	address: string;
}

export type ContinuousRegistrationResult =
	{ ok: true; rows: ContinuousRegistrationRow[]; tags: TagInput[] } | { ok: false; error: string };

/**
 * 連続登録のパターン展開本体。エラーは例外を投げず判別共用体で返す —
 * 呼び出し元（連続登録フォーム）はプレビュー欄にそのまま表示するだけで
 * よい。
 */
export function generateContinuousTags(
	params: ContinuousRegistrationParams
): ContinuousRegistrationResult {
	if (!Number.isInteger(params.count) || params.count < 1) {
		return { ok: false, error: '点数は1以上の整数で指定してください。' };
	}
	if (params.count > MAX_CONTINUOUS_COUNT) {
		return { ok: false, error: `点数は${MAX_CONTINUOUS_COUNT}以下で指定してください。` };
	}

	const trimmedAddress = params.startAddress.trim();
	if (trimmedAddress === '') {
		return { ok: false, error: '開始アドレスを入力してください。' };
	}
	if (hasBitSuffix(trimmedAddress)) {
		return {
			ok: false,
			error:
				'ビット指定アドレス（例: D100.5、40001.3）の連続登録は現時点では未対応です。個別に登録してください。'
		};
	}
	if (params.dataType === 'string' && (!params.stringLength || params.stringLength < 1)) {
		return { ok: false, error: 'string 型では文字列長の指定が必要です。' };
	}
	if (params.namePattern.trim() === '') {
		return { ok: false, error: '名前パターンを入力してください。' };
	}

	const step = addressIncrement(params.dataType, params.stringLength);
	const rows: ContinuousRegistrationRow[] = [];
	const seenNames = new Set<string>();

	for (let i = 0; i < params.count; i++) {
		const n = params.startNumber + i;
		const name = expandNamePattern(params.namePattern, n);
		if (name.trim() === '') {
			return {
				ok: false,
				error: `${i + 1}件目（連番${n}）の名前が空になります。名前パターンを見直してください。`
			};
		}
		if (seenNames.has(name)) {
			return {
				ok: false,
				error: `名前 "${name}" が連番の結果重複します。名前パターンまたは開始番号を見直してください。`
			};
		}
		seenNames.add(name);

		const address = incrementAddress(trimmedAddress, step, i);
		if (address === null) {
			return {
				ok: false,
				error: `開始アドレス "${trimmedAddress}" は自動採番に対応していない形式です（末尾が10進数字のアドレスのみ対応、16進デバイス番号は非対応）。`
			};
		}
		rows.push({ name, address });
	}

	const tags: TagInput[] = rows.map((row) => ({
		name: row.name,
		collectionGroupId: params.collectionGroupId,
		address: row.address,
		dataType: params.dataType,
		stringLength: params.dataType === 'string' ? params.stringLength : undefined,
		rawLo: params.rawLo,
		rawHi: params.rawHi,
		engLo: params.engLo,
		engHi: params.engHi,
		unit: params.unit,
		decimals: params.decimals,
		thresholdH: params.thresholdH,
		thresholdHh: params.thresholdHh,
		thresholdL: params.thresholdL,
		thresholdLl: params.thresholdLl,
		enabled: params.enabled,
		writable: params.writable
	}));

	return { ok: true, rows, tags };
}
