/**
 * R2a-2b（docs/banto-hub-ingest-design.md §5・§12 Q9、2026-10-21 オーナー
 * 決定）: API キーのスコープ `ingest:#{接続 id}` の組み立て・読み取りと、
 * 一覧の表示（純関数のみ、依存ゼロ）。
 *
 * `ingest:#{id}` は `push` 接続へ値を送る権限で、接続の**名前ではなく id**
 * で縛る（改名してもキーは通り続け、削除した接続の id は再利用されない）。
 * 文法はサーバーの `apps/banto-hub/core/src/api_keys.rs` の
 * `parse_ingest_scope` と同じ正規形（`[1-9][0-9]*`）。発行時の検証はサーバー
 * が行い、ここは画面の表示とフォームの組み立てにだけ使う（認可には関与
 * しない）。
 *
 * 一覧の文言は MCP の `list_api_keys` の `ingestTargets`
 * （`apps/banto-hub/core/src/mcp.rs` の `ingest_targets`）と同じ:
 *
 * - 接続があり `push`: 「取り込み: Bridge1（#12）」（今の名前）
 * - 接続はあるが `push` でない: 「取り込み: line1（#3、push 接続ではありません）」
 * - 接続が無い: 「取り込み: （削除済み #12）」（そのキーは送れない）
 * - 接続の一覧を読めなかった: 「取り込み: #12（接続の一覧を読めませんでした）」
 *   （削除済みと取り違えない）
 *
 * `apiKeyWarnings.ts` と同じ理由で `apiKeysAdmin.ts`（`@banto/admin-core` を
 * import する）から切り出してあり、`apiKeysAdmin.ts` が re-export する。
 */

/** `ingest:#{id}` の接頭辞。mirrors `banto_hub_core::api_keys::INGEST_SCOPE_PREFIX`。 */
export const INGEST_SCOPE_PREFIX = 'ingest:#';

/** mirrors `banto_tags::PUSH_PROTOCOL`（`plc_connections.protocol` の `push`）。 */
export const PUSH_PROTOCOL = 'push';

/**
 * 表示とフォームが読む接続の最小の形。`tagRegistryAdmin.ts` の
 * `PlcConnection` は構造的にこれを満たす（`protocol` は文字列として比べる -
 * `PlcProtocol` に `push` を足すのは接続のフォームの段、R2a-2e）。
 */
export interface ScopeConnection {
	id: number;
	name: string;
	protocol: string;
}

const INGEST_SCOPE_PATTERN = /^ingest:#([1-9][0-9]*)$/;

/** 接続 id から `ingest:#{id}` を組み立てる。 */
export function ingestScope(connectionId: number): string {
	return `${INGEST_SCOPE_PREFIX}${connectionId}`;
}

/**
 * `ingest:#{id}` の `{id}` を取り出す。正規形でなければ `null`。サーバーは
 * `i64` の範囲まで受けるが、ここでは JavaScript の数で正確に表せる範囲
 * （`Number.MAX_SAFE_INTEGER` 以下）だけを id として返し、それを超えるもの
 * は `null`（一覧ではスコープの文字列をそのまま出す）。
 */
export function parseIngestScope(scope: string): number | null {
	const match = INGEST_SCOPE_PATTERN.exec(scope);
	if (!match) return null;
	const id = Number(match[1]);
	return Number.isSafeInteger(id) ? id : null;
}

/** 作成のフォームで選べる接続（`push` だけ、元の並びのまま）。 */
export function pushConnectionOptions(connections: readonly ScopeConnection[]): ScopeConnection[] {
	return connections.filter((c) => c.protocol === PUSH_PROTOCOL);
}

/**
 * 作成フォームで選択中の接続 id を、今の接続の一覧に合わせて整える。
 * `connections` が `null`（一覧を読めなかった = チェックボックスが出ない）
 * なら空、そうでなければ「一覧にあり、かつ `push`」の id だけを選んだ順の
 * まま残す。見えない・外せない選択が `ingest:#{id}` として発行されるのを防ぐ
 * （接続の削除・protocol 変更・一覧の取得失敗の後も選択が残る事故）。
 */
export function reconcileIngestSelection(
	selected: readonly number[],
	connections: readonly ScopeConnection[] | null
): number[] {
	if (connections === null) return [];
	const pushIds = new Set(pushConnectionOptions(connections).map((c) => c.id));
	return selected.filter((id) => pushIds.has(id));
}

/**
 * 一覧の 1 つのスコープの表示。`ingest:#{id}` は `connections`（`null` = 一覧を
 * 読めなかった）から今の名前を引いて上のモジュール doc comment の文言にする。
 * それ以外のスコープは文字列をそのまま返す。
 */
export function describeApiKeyScope(
	scope: string,
	connections: readonly ScopeConnection[] | null
): string {
	const id = parseIngestScope(scope);
	if (id === null) return scope;
	if (connections === null) return `取り込み: #${id}（接続の一覧を読めませんでした）`;
	const connection = connections.find((c) => c.id === id);
	if (connection === undefined) return `取り込み: （削除済み #${id}）`;
	if (connection.protocol !== PUSH_PROTOCOL) {
		return `取り込み: ${connection.name}（#${id}、push 接続ではありません）`;
	}
	return `取り込み: ${connection.name}（#${id}）`;
}
