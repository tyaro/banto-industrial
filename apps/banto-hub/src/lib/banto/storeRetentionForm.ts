/**
 * T19 S2-d（UX-39、docs/banto-hub-t19-design.md §5.1、2026-09-03 オーナー
 * 決定）: 履歴の保持期間フォームの純関数群（`registryCascadeImpact.ts`/
 * `tagDeleteImpact.ts` と同じ「依存ゼロの純関数を切り出して vitest で
 * テストする」方針）。`+page.svelte`から呼ばれる。
 *
 * オーナー決定1の核心（`apps/banto-hub/core/src/rest.rs`の
 * `store_settings_put`のdoc comment参照）: 「保存」は保持方針を保存する
 * だけで剪定しない。「今すぐ古い履歴を削除」は保存済みの方針で**別に**
 * 実行する破壊的操作。この2つを混同させないため、
 * - [`hasUnsavedRetentionChange`]: フォームの現在値が保存済みの値と
 *   一致するかを判定する（不一致なら削除ボタンを無効化し「先に保存して
 *   ください」と伝える - 未保存の日数で剪定されると誤解を生むため）。
 * - [`formatPruneConfirmMessage`]: 削除確認ダイアログの文言（不可逆で
 *   あることを明示する - 実装指示どおり）。
 * をここに切り出す。
 */

/** 保持期間フォームの入力値。 */
export interface RetentionFormState {
	/** true なら「無制限（削除しない）」を選択中。 */
	unlimited: boolean;
	/** `unlimited` が false のときだけ意味を持つ保持日数の入力値。 */
	days: number;
}

/** サーバー側 `MAX_STORE_RETENTION_DAYS`（10年）と同じ上限。 */
export const MAX_RETENTION_DAYS = 3650;

/**
 * 保存前のクライアント側バリデーション（サーバー側
 * `validate_store_settings_request`と同じ範囲 - 実装指示「上限は3650
 * （10年）とする」・「0以下は今回 UI から送らせない」）。エラーが無ければ
 * `null`。
 */
export function validateRetentionForm(form: RetentionFormState): string | null {
	if (form.unlimited) return null;
	if (!Number.isInteger(form.days) || form.days < 1 || form.days > MAX_RETENTION_DAYS) {
		return `保持日数は1〜${MAX_RETENTION_DAYS}の整数、または無制限を選んでください`;
	}
	return null;
}

/** フォーム状態を `setStoreSettings`/サーバーの `retentionDays` 表現へ変換する。 */
export function formToRetentionDays(form: RetentionFormState): number | null {
	return form.unlimited ? null : form.days;
}

/** サーバーから読み込んだ `retentionDays` をフォーム状態へ変換する。 */
export function retentionDaysToForm(
	retentionDays: number | null,
	fallbackDays: number
): RetentionFormState {
	return retentionDays === null
		? { unlimited: true, days: fallbackDays }
		: { unlimited: false, days: retentionDays };
}

/**
 * フォームの現在値が**保存済みの**方針と一致するか。「今すぐ古い履歴を
 * 削除」ボタンは、未保存の変更があるときは無効化する（実装指示「このボタン
 * は保存済みの方針で剪定するため。未保存の日数で剪定されると誤解を生む」）。
 */
export function hasUnsavedRetentionChange(saved: number | null, form: RetentionFormState): boolean {
	return saved !== formToRetentionDays(form);
}

/** 未保存の変更があるときに削除ボタンへ出す理由文言。無ければ `null`。 */
export function pruneDisabledReason(hasUnsavedChange: boolean): string | null {
	return hasUnsavedChange ? '先に保存してください（保存済みの保持方針で削除します）' : null;
}

/**
 * 削除確認ダイアログの文言（`window.confirm`用、実装指示どおり不可逆で
 * あることを明示する）。件数0のときは呼び出し側が確認自体をスキップし
 * 「削除対象はありません」と伝える設計なので、ここでは1件以上の場合のみ
 * 想定する。
 */
export function formatPruneConfirmMessage(wouldDeleteCount: number): string {
	return (
		`古い履歴 ${wouldDeleteCount}件を削除します。` +
		'記録済みでも保持期間を過ぎたデータは戻せません。'
	);
}

/** 保存成功時のトースト文言（PR #135 の教訓 - 既存トーストと部分文字列が衝突しないこと）。 */
export function formatRetentionSavedMessage(): string {
	return '保持期間を保存しました（次回の自動剪定から反映されます）';
}

/** 剪定成功時のトースト文言。 */
export function formatPruneDoneMessage(deletedCount: number): string {
	return `古い履歴を削除しました（${deletedCount}件）`;
}
