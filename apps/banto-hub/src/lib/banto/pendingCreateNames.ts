/**
 * 実機で再現した不具合の修正1（2026-08-31、オーナー報告）: 収集稼働中に
 * 収集グループ/PLC接続を新規作成すると、変更は `POST .../plc-connections`
 * や `POST .../collection-groups` が 202 を返して pending queue に積まれる
 * だけで DB には現れない（`rest.rs::queue_pending_registry_change`）。
 * ところが `collectionGroupForm.ts::nextGroupName` /
 * `plcConnectionForm.ts::nextConnectionName` の連番プリフィルは既存レコード
 * （DB上の値）しか見ていなかったため、収集稼働中に同じ Drawer を複数回開くと
 * 毎回同じ名前（例: `group1`）が提案され、後から一括適用すると名前の一意
 * 制約で全滅する（オーナーが収集稼働中に3回作成 → 3回とも `group1` が
 * 提案され、適用時に3件とも validation failed）。
 *
 * ここでは pending queue（`pendingChangesAdmin.ts::PendingChange`）から、
 * まだ適用されていない「作成」分の名前だけを抽出する純関数を提供する。
 * `collectionGroupForm.ts`/`plcConnectionForm.ts` と同じ「依存ゼロ」方針
 * （両ファイルの冒頭コメント参照 - `@banto/admin-core`（Svelte 5 rune を
 * 使う `.svelte.ts`）を推移的に import すると、この最小 vitest 構成では
 * `ReferenceError: $state is not defined` になる）を保つため、
 * `pendingChangesAdmin.ts` の `PendingChange` 型そのものには依存せず、
 * 必要な形だけを構造的部分型として定義する（`apiKeyWarnings.ts` と同じ
 * パターン）。
 */

/**
 * {@link pendingCreateNames} が読む最小の形。`pendingChangesAdmin.ts` の
 * `PendingChange` は構造的にこれを満たすので、呼び出し側はそのまま渡せる。
 */
export interface PendingChangeLike {
	state: string;
	source: string;
	payload: unknown;
}

/**
 * `pending` のうち、まだ適用されていない（`state === 'pending'`）
 * `source` の作成分から、payload の `name` を取り出す。
 *
 * - `applying`/`applied`/`canceled`/`failed` は対象外にする -
 *   `applied` は既に既存レコード側（DB）に現れているので二重に候補へ
 *   入れる必要が無く、`canceled`/`failed` は名前を占有していない
 *   （キャンセル済み・失敗済みの提案は再利用可能な名前を残す）。
 * - `source` が一致しない pending（`collection_groups.update`/`.delete`
 *   や `plc_connections.*` など別リソース/別操作）は対象外 - `update`/
 *   `delete` は新しい名前を占有しない。
 * - payload の形が想定と違う（`{ input: { name } }` -
 *   `rest.rs::plc_connections_create`/`collection_groups_create` が
 *   `queue_pending_registry_change` に渡す形と一致しない）場合は、その
 *   エントリは黙って読み飛ばす（プリフィルは利便性機能なので、ここで例外を
 *   投げて呼び出し側の採番自体を止めない）。
 */
export function pendingCreateNames(
	pending: readonly PendingChangeLike[],
	source: string
): string[] {
	const names: string[] = [];
	for (const change of pending) {
		if (change.state !== 'pending') continue;
		if (change.source !== source) continue;
		const name = extractInputName(change.payload);
		if (name !== undefined) names.push(name);
	}
	return names;
}

function extractInputName(payload: unknown): string | undefined {
	if (typeof payload !== 'object' || payload === null) return undefined;
	const input = (payload as { input?: unknown }).input;
	if (typeof input !== 'object' || input === null) return undefined;
	const name = (input as { name?: unknown }).name;
	return typeof name === 'string' ? name : undefined;
}
