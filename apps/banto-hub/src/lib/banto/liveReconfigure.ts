/**
 * #341（オーナー決定 2026-09-09 / 2026-09-14、レビュー対応2 で共有モジュール
 * 化）: サーバーが返す `500 live_reconfigure_failed` の**唯一の**クライアント
 * 側解釈。
 *
 * この 500 は「**レジストリへの保存は成功したが、走行中の収集へ反映できな
 * かった**」だけを意味する（`apps/banto-hub/core/src/rest.rs` の
 * `LIVE_APPLY_RECOVERY_HINT` / `commit_catalog_and_notify`）。通常の失敗
 * （バリデーション・競合・権限）と違って**操作自体は取り消されていない**
 * ので、「保存できませんでした」と読める汎用の `500 Internal Server Error`
 * を出すと、利用者が同じ操作をやり直して二重に作ってしまう。サーバー側の
 * `message` には保存済みである旨と回復手段（次の構成変更 / 収集の停止→開始 /
 * `POST /api/collection/reapply`）が入っているので、それをそのまま見せる。
 *
 * レジストリ CRUD（`tagRegistryAdmin.ts`）と未適用キューの適用
 * （`pendingChangesAdmin.ts`）の両方がこの 500 を返しうるため、判定と
 * 変換をここ1箇所に置いて二重実装しない。
 */
import { ProviderError } from '@banto/admin-core';

/** サーバー側の機械可読コード（`rest.rs` の `"live_reconfigure_failed"`）。 */
export const LIVE_RECONFIGURE_FAILED = 'live_reconfigure_failed';

/**
 * `500 live_reconfigure_failed` なら、サーバーの `message` をそのまま載せた
 * `ProviderError` を返す。そうでなければ `undefined`（呼び出し元の通常の
 * エラー処理へ委ねる）- `tagRegistryAdmin.ts`/`pendingChangesAdmin.ts` の
 * `httpRequest` が持つ `mapErrorBody` フックと同じ契約。
 */
export function mapLiveReconfigureFailure(body: unknown, status: number): Error | undefined {
	if (status !== 500 || typeof body !== 'object' || body === null) return undefined;
	const candidate = body as { error?: unknown; message?: unknown };
	if (candidate.error !== LIVE_RECONFIGURE_FAILED || typeof candidate.message !== 'string') {
		return undefined;
	}
	return new ProviderError({ kind: 'other', message: candidate.message });
}
