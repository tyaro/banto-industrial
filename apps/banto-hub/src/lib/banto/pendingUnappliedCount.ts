/**
 * ヘッダー/サイドバーの「未適用 N件」バッジ（`(app)/+layout.svelte`）向けの
 * 依存ゼロの純関数。`pendingCreateNames.ts` と同じ「pending queue から
 * 必要な形だけを構造的部分型として受け取る」パターンに倣う。
 *
 * **実機で見つかった不具合（2026-08-31、オーナー報告）**: pending change が
 * 4件あり、内訳は3件がキャンセル済み・1件が適用済み（＝未適用は0件）
 * にもかかわらず、バッジは「未適用 4件」と表示されていた。原因は
 * `+layout.svelte` が `PendingChangeState` による絞り込み無しに
 * `pendingChanges.length` をそのまま数えていたこと。
 *
 * ここでは「未適用」として数える state を明示し、単体テストで固定する。
 *
 * - **`pending`（適用待ち）・`applying`（適用処理中）を数える** - どちらも
 *   まだ確定しておらず、設定に反映されるかどうかがユーザーの操作待ちの
 *   状態であるため。
 * - **`applied`（適用済み）は数えない** - 既に設定へ反映済みで、ユーザーの
 *   対応は不要。
 * - **`canceled`（キャンセル済み）は数えない** - ユーザー（または処理）が
 *   既に「適用しない」と確定させた終端状態で、対応不要。
 * - **`failed`（失敗）も数えない** - 迷いどころではあるが、
 *   `status/+page.svelte` の Pending changes カードでは `failed` に専用の
 *   `state-chip state-bad`（`canceled` と同じ「bad」分類）と「再試行」
 *   導線が既に用意されており、そちらで十分に目立つ形で扱われている。
 *   このバッジは「まだ判定が下っていない件数」を示す用途と考えられるため、
 *   判定済み（成功/失敗とも）の終端状態は含めない保守的な選択にした。
 */

/** {@link countUnappliedPendingChanges} が読む最小の形。 */
export interface PendingChangeStateLike {
	state: string;
}

/** バッジ上で「未適用」として扱う state の集合。 */
const UNAPPLIED_STATES: ReadonlySet<string> = new Set(['pending', 'applying']);

/**
 * pending queue のうち、まだ確定していない（`pending`/`applying`）件数を
 * 数える。`applied`/`canceled`/`failed` は含めない（理由は本ファイル冒頭を
 * 参照）。
 */
export function countUnappliedPendingChanges(
	pendingChanges: readonly PendingChangeStateLike[]
): number {
	let count = 0;
	for (const change of pendingChanges) {
		if (UNAPPLIED_STATES.has(change.state)) count += 1;
	}
	return count;
}
