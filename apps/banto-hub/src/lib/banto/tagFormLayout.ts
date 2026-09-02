/**
 * T18-2a（docs/banto-hub-t18-design.md「T18-2a 単票フォーム刷新」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-B）: `tags/+page.svelte` の
 * 単票 create/edit フォーム（`tagFields` スニペット）刷新に必要な、
 * 依存ゼロの純関数・定数だけを切り出す。`tagFormNumeric.ts`/
 * `tagDeleteImpact.ts`/`formDirty.ts` と同じ方針 — Svelte 側は `$state`/
 * `$effect`/DOM 組み立てに専念させ、ここに置く関数はスナップショット値
 * だけを引数に取り、テストしやすく保つ。
 *
 * 本モジュールが担う3つの役割:
 *
 * 1. **詳細セクションの自動展開判定**（TAG-UX-B「詳細側にエラー時は自動
 *    展開する」）: `<details class="detail-group">` 3セクション
 *    （表示・スケーリング／しきい値／書き込み安全設定）のどれにエラーが
 *    出ているかを判定する {@link hasFieldError} と、そのためのフィールド
 *    集合定数（{@link DISPLAY_SCALING_FIELDS} など）。
 * 2. **保存前の固定確認領域**（TAG-UX-B「保存前に最終外部名
 *    `{connection}.{group}.{tag}`、実機／SIM、書き込み許可を固定領域で
 *    確認できるようにする」）: {@link buildConfirmExternalName}・
 *    {@link environmentLabel}・{@link writePermissionLabel}。
 *    `+page.svelte` 側は `groups`/`connections` から接続・グループ名を
 *    引いてこれらへ渡すだけの薄いラッパーを持つ（ページ側の `FormState`/
 *    `groupsFor` 等に依存させないため、ここでは素の文字列・真偽値だけを
 *    受け取る）。
 * 3. **詳細セクションの「値設定済み」インジケータ**（T19 S1-b UX-36、
 *    docs/banto-hub-t19-design.md §2「スケーリング・閾値は詳細設定に
 *    格納。既定は閉じた状態。値が設定されているときは、閉じていても
 *    それが分かるようにする」、2026-09-02 オーナー決定）:
 *    {@link hasAnyFieldValue} と、インジケータ対象のフィールド集合定数
 *    （{@link DISPLAY_SCALING_VALUE_FIELDS}）。「閉じたら値が見えなくなり、
 *    危険なしきい値設定に気付けない」という安全上の懸念（design原文）への
 *    対応 - `<summary>` にバッジを出す判定に使う。
 *
 * 上記いずれも `FormState`（ページ側の型）を直接知らなくても成立するため
 * ここへ切り出すことでユニットテスト対象にできる（`+page.svelte` 自体には
 * 自動テストが無い）。
 */
import type { TagKind } from './tagRegistryAdmin';

/** 「表示・スケーリング」詳細セクションに属するフィールド名。 */
export const DISPLAY_SCALING_FIELDS = ['decimals', 'rawLo', 'rawHi', 'engLo', 'engHi'] as const;

/** 「しきい値」詳細セクションに属するフィールド名。 */
export const THRESHOLD_FIELDS = ['thresholdH', 'thresholdHh', 'thresholdL', 'thresholdLl'] as const;

/** 「書き込み安全設定」詳細セクションに属するフィールド名。 */
export const WRITE_SAFETY_FIELDS = ['writable'] as const;

/**
 * T19 S1-b（UX-36）: 「表示・スケーリング」の「値設定済み」インジケータ
 * 対象。{@link DISPLAY_SCALING_FIELDS} から `decimals` を除いたもの -
 * `decimals` は常に既定値 `'0'`（未入力ではなく通常の数値入力）を持つ
 * フィールドで、design が挙げる対象（「RawLo/Hi・EngLo/Hi・閾値
 * HH/H/L/LL」）にも含まれないため、バッジ判定からは外す（「値が入って
 * いれば知らせる」の対象は、入っていないのが普通というフィールドに絞る）。
 */
export const DISPLAY_SCALING_VALUE_FIELDS = ['rawLo', 'rawHi', 'engLo', 'engHi'] as const;

/**
 * `errors`（フィールド名 → エラーメッセージのマップ）のうち、`fields` に
 * 含まれるいずれかのキーが真値を持つか。`<details bind:open>` を強制的に
 * 開く条件の判定に使う（TAG-UX-B「詳細側にエラー時は自動展開する」）。
 */
export function hasFieldError(errors: Record<string, string>, fields: readonly string[]): boolean {
	return fields.some((field) => Boolean(errors[field]));
}

/**
 * T19 S1-b（UX-36、モジュール冒頭コメントの役割3参照）: `values` のうち、
 * `fields` に含まれるいずれかが空でない値を持つか。`<details>` を閉じた
 * ままでも `<summary>` にバッジを出すかどうかの判定に使う。
 *
 * `values` は広い `object` 型で受ける（関数内部の doc comment 参照 -
 * `Record<string, unknown>` にすると呼び出し元の具体的なインターフェース
 * 型で「index signature が無い」という TypeScript エラーになる）。
 * これにより単票タグフォーム（`FormState`、数値系フィールドはすべて
 * `string`、「空文字列 = 未設定」規約、`blankForm`/`numOrEmpty` 参照）と
 * 連続登録フォーム（`ContinuousFormState`、Svelte 5 の
 * `<input type="number" bind:value>` が代入する実体は
 * `string | number | null`、`continuousRegistration.ts` の doc comment
 * 参照）の両方から同じ関数で呼べる - 個別に型を合わせた薄いラッパーを
 * 2つ持つより、値を文字列化して判定する方が単純。`null`/`undefined` は
 * 未設定として扱い、それ以外は文字列化して前後の空白を trim する
 * （`buildConfirmExternalName` の trim 方針と同じ）。
 */
export function hasAnyFieldValue(values: object, fields: readonly string[]): boolean {
	// `values` is typed as the broad `object` (rather than `Record<string,
	// unknown>`) precisely so callers can pass concrete interfaces
	// (`FormState`/`ContinuousFormState`, neither of which declares an index
	// signature) without a TypeScript "index signature is missing" error at
	// the call site - `Record<string, unknown>` requires the argument type
	// to structurally have one. The cast here is safe: every field this
	// function is ever called with is a known key of the caller's own form
	// type (`DISPLAY_SCALING_VALUE_FIELDS`/`THRESHOLD_FIELDS`), so indexing
	// with a lookup that might miss is exactly the same risk a direct
	// `values[field]` would already have.
	const record = values as Record<string, unknown>;
	return fields.some((field) => {
		const v = record[field];
		return v !== null && v !== undefined && String(v).trim() !== '';
	});
}

/**
 * 保存前確認領域の「外部名」行 `{connection}.{group}.{tag}` を組み立てる。
 * `tagDeleteImpact.ts::buildExternalName` と異なり、こちらは**保存前・
 * 入力途中**のフォーム値が対象なので、接続/グループ未選択やタグ名未入力を
 * エラーにせず、それぞれ分かりやすいプレースホルダで埋める。
 */
export function buildConfirmExternalName(input: {
	connectionName?: string;
	groupName?: string;
	tagName: string;
}): string {
	const conn =
		input.connectionName && input.connectionName.trim() !== '' ? input.connectionName : '(未選択)';
	const group = input.groupName && input.groupName.trim() !== '' ? input.groupName : '(未選択)';
	const trimmedName = input.tagName.trim();
	const name = trimmedName !== '' ? trimmedName : '(未入力)';
	return `${conn}.${group}.${name}`;
}

/**
 * 保存前確認領域の「実機 / SIM」行。`simulation` はフォームが参照する
 * 収集グループの接続（`PlcConnection.simulation`）— グループ未選択の間は
 * `undefined` になる。
 */
export function environmentLabel(simulation: boolean | undefined): string {
	if (simulation === undefined) return '-';
	return simulation ? 'シミュレーション（SIM）' : '実機';
}

/**
 * 保存前確認領域の「書き込み許可」行。`computed` タグは式が値を決めるため
 * 常に書き込み不可（`toInput` が送信直前にも強制する規則と同じ、
 * `+page.svelte` の `toInput` コメント参照）— フォームのチェックボックス値
 * に関わらずここでも「不許可」で表示を揃える。
 */
export function writePermissionLabel(tagKind: TagKind, writable: boolean): string {
	if (tagKind === 'computed') return '不許可（演算タグは書き込み不可）';
	return writable ? '許可' : '不許可';
}

/**
 * T18-2b（TAG-UX-6「入力中に共通 preflight を実行する」）: サーバーが返す
 * フィールドエラーの配列（`field`/`message` のペア）をフィールド名 →
 * メッセージのマップへ畳み込む。もともと単票 create/edit の submit 時
 * エラー処理（`+page.svelte::applyFieldErrors`）だけが持っていたロジック
 * だが、単票の dry-run preflight（`createTagsBatch(..., dryRun=true)` の
 * `BatchTagsResult.errors[n].fieldErrors`）も同じ `{field, message}[]` の
 * 形（`@banto/admin-core::FieldError` / `tagRegistryAdmin.ts::BatchTagFieldError`
 * と同型）を返すため、入力中プレビューと submit 時エラー表示の両方から
 * 呼べるようここへ切り出した。
 *
 * TAG-P0-2 の preflight（`field: "configuration"` に全体エラーをまとめる
 * 契約、`apps/banto-hub/core/src/rest.rs::preflight_api_error`）はどの
 * 単票フィールドにも属さないため、メッセージに「アドレス」を含む場合は
 * `address` キーにもコピーする（`errors.address` が未設定のときのみ -
 * 将来アドレス欄自体のフィールドエラーが返るようになったら上書きしない）。
 */
export function fieldErrorsFromList(
	fieldErrors: readonly { field: string; message: string }[]
): Record<string, string> {
	const map: Record<string, string> = {};
	for (const fe of fieldErrors) map[fe.field] = fe.message;
	if (map.configuration && !map.address && map.configuration.includes('アドレス')) {
		map.address = map.configuration;
	}
	return map;
}
