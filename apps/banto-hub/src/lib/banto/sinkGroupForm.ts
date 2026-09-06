/**
 * S6（docs/banto-hub-external-db-design.md §5.2・§5.3・§7 row S6）:
 * `SinkGroupDrawer.svelte` が使う、依存ゼロの純関数・定数・フォーム状態の
 * 型だけを切り出したモジュール - `plcConnectionForm.ts`/`collectionGroupForm.ts`
 * と同じ方針（Svelte 側は `$state`/DOM 組み立てに専念させ、ここに置く関数は
 * スナップショット値だけを引数に取り、テストしやすく保つ）。
 *
 * このモジュールが担う3つの役割:
 *
 * 1. **クライアント側検証**（{@link validateSinkGroupForm}）:
 *    `apps/banto-hub/core/src/sink/service.rs::validate_sink_group_input`
 *    の同期チェック（DB を触らない部分）だけをミラーする。DB 参照が要る
 *    非同期チェック（接続の存在・`protocol == postgres`・タグの存在・名前
 *    重複）はミラーしない - サーバー側 422 応答（`FieldError`）に委ねる
 *    （`plcConnectionForm.ts::validatePostgresFields`の doc comment と同じ
 *    「クライアント側検証はサーバー側のルールを反映するが全部は複製しない」
 *    方針）。
 * 2. **推奨 DDL の組み立て**（{@link buildRecommendedDdl}）:
 *    `apps/banto-hub-sink/src/sql.rs::recommended_ddl` と**同じ出力**になる
 *    よう1バイトも変えずに移植する（設計 §5.3「Hub の UI には推奨 DDL を
 *    表示する」・§6-7）。ユニットテスト（`sinkGroupForm.test.ts`）で
 *    `recommended_ddl_quotes_the_table_and_matches_the_design_schema`と同じ
 *    入力（`t`・`s.t`）に対して同じ出力になることを確認する。
 * 3. **フォーム状態の組み立て**（{@link blankSinkGroupForm}/
 *    {@link sinkGroupToForm}/{@link formToSinkGroupInput}）。
 *
 * **vitest 制約の回避について**（`tagRegistryAdmin.test.ts`のdoc comment
 * 「vitest 制約の回避について」・`plcConnectionForm.ts`冒頭のdoc comment
 * と同じ理由）: `sinkGroupsAdmin.ts`は`@banto/admin-core`/`./setup`を
 * トップレベルで import する（Svelte 5 rune を使うモジュールへ連鎖する）
 * ため、そちらを**値として** import すると、このモジュール単体のテスト
 * （`sinkGroupForm.test.ts`、`vi.mock`無し）が`ReferenceError: $state is
 * not defined`で落ちる。`SinkGroup`/`SinkGroupInput`/`SinkGroupMode`は
 * **型としてのみ** import し、値の定数（`ALLOWED_SINK_MODES`等）はここに
 * 複製する。
 */
import type { SinkGroup, SinkGroupInput, SinkGroupMode } from './sinkGroupsAdmin';

/** mirrors `sinkGroupsAdmin.ts::ALLOWED_SINK_MODES`（上の doc comment参照 - 値としての複製）。 */
const ALLOWED_SINK_MODES: readonly SinkGroupMode[] = ['interval', 'on_change'];
/** mirrors `sinkGroupsAdmin.ts::MIN_SINK_INTERVAL_MS`。 */
const MIN_SINK_INTERVAL_MS = 100;
/** mirrors `sinkGroupsAdmin.ts::MAX_SINK_INTERVAL_MS`。 */
const MAX_SINK_INTERVAL_MS = 3_600_000;
/** mirrors `sinkGroupsAdmin.ts::MAX_SINK_GROUP_NAME_LEN`。 */
const MAX_SINK_GROUP_NAME_LEN = 64;

/**
 * `hub_sink_groups.table_name`の識別子セグメント検証 - mirrors
 * `apps/banto-hub/core/src/sink/service.rs::is_valid_table_identifier_segment`
 * / `apps/banto-hub-sink/src/sql.rs::is_valid_identifier_segment`（両者は
 * 同じ規則: ASCII 英字か `_` で始まり、以後 ASCII 英数字/`_`、最大63バイト）。
 */
const MAX_TABLE_IDENTIFIER_LEN = 63;

function isValidTableIdentifierSegment(segment: string): boolean {
	if (segment.length === 0 || segment.length > MAX_TABLE_IDENTIFIER_LEN) return false;
	if (!/^[A-Za-z_]/.test(segment)) return false;
	return /^[A-Za-z0-9_]+$/.test(segment);
}

/**
 * `table`または`schema.table`（`.`区切りで最大2セグメント）- mirrors
 * `is_valid_table_name`（Hub 側`sink/service.rs`・サイドカー側`sql.rs`の
 * 両方が同じ規則を持つ、`sql.rs`のモジュール doc「サーバー側の検証」参照）。
 */
export function isValidSinkTableName(tableName: string): boolean {
	const segments = tableName.split('.');
	if (segments.length === 1) return isValidTableIdentifierSegment(segments[0]);
	if (segments.length === 2) {
		return isValidTableIdentifierSegment(segments[0]) && isValidTableIdentifierSegment(segments[1]);
	}
	return false;
}

/**
 * 検証済みのテーブル名をダブルクォートで引用する - mirrors
 * `apps/banto-hub-sink/src/sql.rs::quote_table_name`。**検証を通っていない
 * 文字列を渡してはいけない**（{@link buildRecommendedDdl}が唯一の呼び出し元）。
 */
function quoteTableName(tableName: string): string {
	return tableName
		.split('.')
		.map((segment) => `"${segment}"`)
		.join('.');
}

/**
 * 推奨 DDL - mirrors `apps/banto-hub-sink/src/sql.rs::recommended_ddl`
 * （長スキーマの5列、設計 §5.3）。**サーバー側と1バイトも違わない文面**に
 * すること - `sinkGroupForm.test.ts`が同じ入力に対する Rust 側テスト
 * （`sql.rs::recommended_ddl_quotes_the_table_and_matches_the_design_schema`）
 * と同じ出力になることを検証する。
 */
export function buildRecommendedDdl(tableName: string): string {
	return (
		`CREATE TABLE ${quoteTableName(tableName)} (ts timestamptz NOT NULL, ` +
		'tag_id bigint NOT NULL, external_name text NOT NULL, value double precision, ' +
		'quality text NOT NULL)'
	);
}

/** 編集フォーム状態（作成/編集共通）。数値入力は文字列で保持し、空欄=未設定。 */
export interface SinkGroupFormState {
	name: string;
	/** 空文字列 = 未選択。`Number(dbConnectionId)`で送信直前に数値化する。 */
	dbConnectionId: string;
	mode: SinkGroupMode;
	intervalMs: string;
	tableName: string;
	storeBad: boolean;
	enabled: boolean;
	tagIds: number[];
}

export function blankSinkGroupForm(): SinkGroupFormState {
	return {
		name: '',
		dbConnectionId: '',
		mode: 'interval',
		intervalMs: '1000',
		tableName: '',
		storeBad: false,
		enabled: true,
		tagIds: []
	};
}

export function sinkGroupToForm(group: SinkGroup): SinkGroupFormState {
	return {
		name: group.name,
		dbConnectionId: String(group.dbConnectionId),
		mode: group.mode,
		intervalMs: String(group.intervalMs),
		tableName: group.tableName,
		storeBad: group.storeBad,
		enabled: group.enabled,
		tagIds: [...group.tagIds]
	};
}

export function formToSinkGroupInput(form: SinkGroupFormState): SinkGroupInput {
	return {
		name: form.name,
		dbConnectionId: Number(form.dbConnectionId),
		mode: form.mode,
		intervalMs: Number(form.intervalMs),
		tableName: form.tableName,
		storeBad: form.storeBad,
		enabled: form.enabled,
		tagIds: [...form.tagIds]
	};
}

/**
 * クライアント側の事前検証 - サーバー側
 * `validate_sink_group_input`の同期チェックだけをミラーする（このモジュール
 * doc「クライアント側検証」節参照）。メッセージはサーバー側
 * （`apps/banto-hub/core/src/sink/service.rs`）と一字一句揃える。
 */
export function validateSinkGroupForm(
	form: Pick<
		SinkGroupFormState,
		'name' | 'dbConnectionId' | 'mode' | 'intervalMs' | 'tableName' | 'tagIds'
	>
): Record<string, string> {
	const errors: Record<string, string> = {};

	const trimmedName = form.name.trim();
	if (trimmedName === '') {
		errors.name = '名前を入力してください';
	} else if (trimmedName.length > MAX_SINK_GROUP_NAME_LEN) {
		errors.name = `名前は${MAX_SINK_GROUP_NAME_LEN}文字以内で入力してください`;
	}

	if (!ALLOWED_SINK_MODES.includes(form.mode)) {
		errors.mode = `mode は次のいずれかを指定してください: ${ALLOWED_SINK_MODES.join(', ')}`;
	}

	const intervalMs = Number(form.intervalMs);
	if (
		!Number.isFinite(intervalMs) ||
		!Number.isInteger(intervalMs) ||
		intervalMs < MIN_SINK_INTERVAL_MS ||
		intervalMs > MAX_SINK_INTERVAL_MS
	) {
		errors.intervalMs = `intervalMs は${MIN_SINK_INTERVAL_MS}〜${MAX_SINK_INTERVAL_MS}の範囲で指定してください`;
	}

	if (!isValidSinkTableName(form.tableName)) {
		errors.tableName =
			'tableName は英字/アンダースコアで始まる識別子（省略可能な1回のスキーマ修飾 `schema.table` を含む）を指定してください。引用符は使用できません';
	}

	if (form.dbConnectionId.trim() === '' || Number.isNaN(Number(form.dbConnectionId))) {
		errors.dbConnectionId = '接続を選択してください';
	}

	if (form.tagIds.length === 0) {
		errors.tagIds = '対象タグを1件以上指定してください';
	}

	return errors;
}

/** モード選択肢（日本語ラベル + 一行説明、設計 §7 row S6 実装指示1）。 */
export const SINK_MODE_OPTIONS: { value: SinkGroupMode; label: string; hint: string }[] = [
	{
		value: 'interval',
		label: 'interval（定周期）',
		hint: '値の変化に関わらず、指定した間隔で毎回 INSERT します。'
	},
	{
		value: 'on_change',
		label: 'on_change（変化時）',
		hint: '値が変化したときだけ INSERT します（指定した間隔は最短発行間隔として働きます）。'
	}
];

/** `intervalMs`欄のラベル - `on_change`では「最短発行間隔」に変える（実装指示1）。 */
export function intervalMsLabel(mode: SinkGroupMode): string {
	return mode === 'on_change' ? '最短発行間隔（ミリ秒）' : '発行間隔（ミリ秒）';
}
