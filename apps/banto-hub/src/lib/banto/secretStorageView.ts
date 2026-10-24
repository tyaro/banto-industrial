/**
 * R2b-1e（docs/banto-hub-connection-settings-design.md §5.5・§5.8・§6.5）:
 * 接続の秘密値（パスワード）まわりの表示文言の純関数。依存ゼロ
 * （`auditSpoolWarning.ts` と同じく vitest の最小構成で読めるようにしている）。
 *
 * R2b-6c: 状態は応答の `settings.password` の `$secret`（平らな `passwordSet`・`passwordState` は
 * 無くなった）。
 */

/** 接続の秘密のキー（postgres の `password`）が `{"$secret": "unreadable"}` のときの文言（サーバーの `lastError` の頭と同じ）。 */
export const PASSWORD_UNREADABLE_MESSAGE = 'パスワード: 読めません（この PC で再入力してください）';

/** 応答の `settings.password` の `$secret` の状態（値が載らなければ `none`）。 */
export type PasswordState = 'none' | 'set' | 'unreadable';

export interface PasswordStatusView {
	/** ドロワーに出す 1 行。 */
	text: string;
	/** `true` なら警告として目立たせる（入れ直して保存すると応答の値で消える）。 */
	unreadable: boolean;
}

/**
 * ドロワーの「パスワード: …」行。応答の `settings.password` の `$secret` の状態
 * （`plcConnectionForm.ts::connectionToForm` が `passwordState` に写す）から作る。
 * 状態が無い（新規・保存前）は「未設定」。
 */
export function passwordStatusView(state: PasswordState | null | undefined): PasswordStatusView {
	if (state === 'unreadable') {
		return { text: PASSWORD_UNREADABLE_MESSAGE, unreadable: true };
	}
	return { text: `パスワード: ${state === 'set' ? '設定済み' : '未設定'}`, unreadable: false };
}

/** `GET /api/status` の `secretStorage`（管理系だけ）。 */
export interface SecretStorageEntry {
	scheme: 'dpapi-machine-v1' | 'plain-v1' | string;
	plaintext_warning: string | null;
	plaintext_remaining: number;
	plaintext_remaining_warning: string | null;
}

/** 状態の画面に出す秘密値の警告。非 null のものだけを順に返す（無ければ空）。 */
export function secretStorageWarnings(
	entry:
		Pick<SecretStorageEntry, 'plaintext_warning' | 'plaintext_remaining_warning'> | null | undefined
): string[] {
	if (!entry) return [];
	return [entry.plaintext_warning, entry.plaintext_remaining_warning].filter(
		(w): w is string => typeof w === 'string' && w !== ''
	);
}
