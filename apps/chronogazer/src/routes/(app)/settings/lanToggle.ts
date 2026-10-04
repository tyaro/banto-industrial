/**
 * LAN アクセスのチェックを操作不可にする条件（PR #499 レビュー P2）。
 *
 * 「ログイン不要モード中は、閲覧公開が ON のときだけ LAN を有効化できる」は
 * **有効化** の制約である。現在 ON のものを止める操作まで塞ぐと、ログイン不要 +
 * LAN + 閲覧公開がすべて ON の状態で閲覧公開を先に外したとき、LAN のチェックが
 * ON のまま動かせなくなり、LAN を止めて適用する道が無くなる（保存は
 * 「ログイン不要 + LAN ON + 閲覧公開 OFF」としてバックエンドが拒否する）。
 * そのため「現在 OFF で、有効化できない」ときだけ塞ぐ。
 */
export function lanToggleLocked(
	authDisabled: boolean | undefined,
	viewerPublicDraft: boolean,
	enabledDraft: boolean
): boolean {
	return Boolean(authDisabled) && !viewerPublicDraft && !enabledDraft;
}
