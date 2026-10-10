/**
 * #437: 状態画面の「書き込み受付」に出す、監査の保留の注意書き（純関数のみ、
 * 依存ゼロ - `apiKeyWarnings.ts` と同じく vitest の最小構成で読めるように
 * `hubStatus.ts` から切り離している）。
 *
 * サーバーは DB に書けない・応答しない監査をデータディレクトリの保留ファイル
 * （`audit-spool/`）に退避し、DB が戻ると自動で監査ログへ流し込む（banto
 * ADR-0019、`apps/banto-hub/core/src/audit_spool.rs`）。`GET /api/status` の
 * `auditPendingCount` 等がその状態で、ここで人が読む 1 文にする。
 */

/** [`auditSpoolWarning`] が読む最小の形（`StatusResponse` は構造的に満たす）。 */
export interface AuditSpoolStatus {
	audit_pending_count: number;
	audit_pending_oldest_ts: string | null;
	audit_dropped_count: number;
	audit_failed_count: number;
}

/**
 * 保留も喪失も無ければ `null`。あれば、保留の件数（と最も古い時刻）、上限を
 * 超えて捨てた件数、保留ファイルにも書けずに失った件数を順に並べた文。
 * `audit_pending_oldest_ts` は `audit_log.ts` と同じ UTC の文字列なので、
 * そのまま「UTC」と添えて出す。
 */
export function auditSpoolWarning(status: AuditSpoolStatus): string | null {
	const parts: string[] = [];
	if (status.audit_pending_count > 0) {
		const oldest = status.audit_pending_oldest_ts
			? `（最も古いもの: ${status.audit_pending_oldest_ts} UTC）`
			: '';
		parts.push(
			`DB に書けなかった監査 ${status.audit_pending_count} 件をデータディレクトリに保留しています${oldest}。DB が戻ると自動で監査ログに書き込みます。`
		);
	}
	if (status.audit_dropped_count > 0) {
		parts.push(
			`保留の上限（10,000 件）を超えたため、監査 ${status.audit_dropped_count} 件を記録できませんでした。`
		);
	}
	if (status.audit_failed_count > 0) {
		parts.push(
			`保留ファイルにも書けなかったため、監査 ${status.audit_failed_count} 件を記録できませんでした。`
		);
	}
	return parts.length > 0 ? parts.join('') : null;
}
