/**
 * R2b-1e（docs/banto-hub-connection-settings-design.md §5.5・§5.8・§6.5）:
 * 接続の秘密値（パスワード）まわりの表示文言の純関数。依存ゼロ
 * （`auditSpoolWarning.ts` と同じく vitest の最小構成で読めるようにしている）。
 *
 * サーバーの欄はどれも新しく足したもので、古いサーバーは返さない。欠けていても
 * 壊れないよう、すべて任意の欄として扱う。
 */

/** 接続の `passwordState: "unreadable"` のときの文言（サーバーの `lastError` の頭と同じ）。 */
export const PASSWORD_UNREADABLE_MESSAGE = 'パスワード: 読めません（この PC で再入力してください）';

/** [`passwordStatusView`] が読む最小の形（`PlcConnection` は構造的に満たす）。 */
export interface PasswordStatusSource {
	passwordSet: boolean;
	passwordState?: 'none' | 'set' | 'unreadable' | string;
}

export interface PasswordStatusView {
	/** ドロワーに出す 1 行。 */
	text: string;
	/** `true` なら警告として目立たせる（入れ直して保存すると応答の値で消える）。 */
	unreadable: boolean;
}

/**
 * ドロワーの「パスワード: …」行。`passwordState` があればそれを優先し、
 * 無い古い応答は `passwordSet` で今までどおり「設定済み/未設定」。
 */
export function passwordStatusView(connection: PasswordStatusSource | null): PasswordStatusView {
	if (connection?.passwordState === 'unreadable') {
		return { text: PASSWORD_UNREADABLE_MESSAGE, unreadable: true };
	}
	const set =
		connection?.passwordState === 'set' ||
		(connection?.passwordState !== 'none' && connection?.passwordSet === true);
	return { text: `パスワード: ${set ? '設定済み' : '未設定'}`, unreadable: false };
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
