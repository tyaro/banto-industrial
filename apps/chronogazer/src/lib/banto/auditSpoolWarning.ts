/**
 * #437: 監査ログ画面に出す、監査の保留の注意書き（純関数のみ、依存ゼロ）。
 *
 * サーバーは DB に書けない・応答しない監査をデータディレクトリの保留ファイル
 * （`audit-spool/`）に退避し、DB が戻ると自動で監査ログへ流し込む（banto
 * ADR-0019、`apps/chronogazer/core/src/audit_spool.rs`）。
 * `GET /api/audit-log/spool` の `pendingCount` 等がその状態で、ここで人が
 * 読む 1 文にする（banto-hub の `auditSpoolWarning.ts` と同じ文言）。
 */

/** [`auditSpoolWarning`] が読む最小の形（`auditLogAdmin.AuditSpoolStatus` は構造的に満たす）。 */
export interface AuditSpoolCounts {
	pendingCount: number;
	pendingOldestTs: string | null;
	droppedCount: number;
	failedCount: number;
}

/**
 * 保留も喪失も無ければ `null`。あれば、保留の件数（と最も古い時刻）、上限を
 * 超えて捨てた件数、保留ファイルにも書けずに失った件数を順に並べた文。
 * `pendingOldestTs` は `audit_log.ts` と同じ UTC の文字列なので、そのまま
 * 「UTC」と添えて出す。
 */
export function auditSpoolWarning(status: AuditSpoolCounts): string | null {
	const parts: string[] = [];
	if (status.pendingCount > 0) {
		const oldest = status.pendingOldestTs ? `（最も古いもの: ${status.pendingOldestTs} UTC）` : '';
		parts.push(
			`DB に書けなかった監査 ${status.pendingCount} 件をデータディレクトリに保留しています${oldest}。DB が戻ると自動で監査ログに書き込みます。`
		);
	}
	if (status.droppedCount > 0) {
		parts.push(
			`保留の上限（10,000 件）を超えたため、監査 ${status.droppedCount} 件を記録できませんでした。`
		);
	}
	if (status.failedCount > 0) {
		parts.push(
			`保留ファイルにも書けなかったため、監査 ${status.failedCount} 件を記録できませんでした。`
		);
	}
	return parts.length > 0 ? parts.join('') : null;
}

/** 状態の取得結果: 読めた（`ok`）か、読めなかった（`failed`）か。 */
export type AuditSpoolOutcome = { kind: 'ok'; status: AuditSpoolCounts } | { kind: 'failed' };

/** 状態を読めなかったときの文。保留が 0 件とは言い切れない、と伝える。 */
export const AUDIT_SPOOL_UNREADABLE_MESSAGE =
	'監査の保留の状態を読めませんでした。DB が応答していない可能性があります（保留が 0 件とは限りません）。「再読み込み」でもう一度試せます。';

/**
 * 画面に出す注意書き。読めたなら [`auditSpoolWarning`]（保留も喪失も無ければ
 * `null`）、読めなかったなら [`AUDIT_SPOOL_UNREADABLE_MESSAGE`]。「読めなかった」
 * と「0 件」を同じ `null` にしない（DB が応答しない障害こそ、この警告が要る場面）。
 */
export function auditSpoolNotice(outcome: AuditSpoolOutcome): string | null {
	return outcome.kind === 'ok' ? auditSpoolWarning(outcome.status) : AUDIT_SPOOL_UNREADABLE_MESSAGE;
}
