/**
 * 楽観ロック（`expectedRevision`）の食い違いの判定。タグ（#525）と表示グループ
 * （#393）で 1 つにそろえたもの（Rust 側は `chronogazer_core::revision`）。
 *
 * サーバーは REST（`409`）も Tauri も、食い違いを `kind: "validation"` の
 * `field_errors` に `field: "expectedRevision"` を載せて返す。依存を持たない
 * （vitest からそのまま読める）。
 */

/** 版の食い違いのときのフィールド名（Rust の `REVISION_CONFLICT_FIELD`）。 */
export const REVISION_CONFLICT_FIELD = 'expectedRevision';

/** 保存の失敗（`ProviderError` など、`body` に `ErrorBody` を持つもの）が版の食い違いか。 */
export function isRevisionConflict(err: unknown): boolean {
	if (typeof err !== 'object' || err === null) return false;
	const body = (err as { body?: { kind?: unknown; field_errors?: unknown } }).body;
	if (!body || body.kind !== 'validation' || !Array.isArray(body.field_errors)) return false;
	return body.field_errors.some(
		(fe) =>
			typeof fe === 'object' &&
			fe !== null &&
			(fe as { field?: unknown }).field === REVISION_CONFLICT_FIELD
	);
}
