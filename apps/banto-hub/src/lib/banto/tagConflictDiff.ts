/**
 * T18-1（TAG-UX-C 4点目「差分表示 UI」、docs/banto-hub-desktop-plan.md
 * §9.4「revision / ETag で後勝ち上書きを防ぎ、競合時は差分を表示する」）:
 * revision 競合検出（`cursor/t18-1-tags-revision-e3cb`）に続く後半部分 -
 * ローカルの編集内容とサーバー最新値をフィールド単位で比較する純関数。
 *
 * `tags/+page.svelte` の `FormState` はページ内 private 型のため、ここでは
 * それに依存しない汎用シグネチャ（`Record<string, unknown>` 同士の比較）
 * にしてある。呼び出し側（ページ）が `FormState` を plain object に変換
 * して渡す。
 */

/** 差分のあった1フィールド（競合パネルの表1行に対応）。 */
export type ConflictFieldDiff = {
	key: string;
	/** 日本語ラベル。`labels` に対応するキーが無ければ `key` をそのまま使う。 */
	label: string;
	/** 表示用に正規化した文字列（{@link displayValue}）。 */
	local: string;
	server: string;
};

/**
 * 値を表示用の読みやすい文字列に正規化する。
 * - `boolean` は「オン」/「オフ」。
 * - 空文字・`null`・`undefined` は「（空）」（未設定であることが分かる表示）。
 * - それ以外は `String(value)`。
 */
function displayValue(value: unknown): string {
	if (typeof value === 'boolean') return value ? 'オン' : 'オフ';
	if (value === null || value === undefined || value === '') return '（空）';
	return String(value);
}

/**
 * `local`/`server` の同型レコードを比較し、値が異なるフィールドだけを
 * 返す（差分が無ければ空配列）。両オブジェクトのキーの和集合を見るため、
 * 一方にしか存在しないキーの差分も検出する。キーの並びは `local` の
 * プロパティ順が先、`server` にしかないキーはその後ろに続く。
 */
export function diffFormRecords(
	local: Record<string, unknown>,
	server: Record<string, unknown>,
	labels: Record<string, string>
): ConflictFieldDiff[] {
	const keys = [...Object.keys(local), ...Object.keys(server).filter((k) => !(k in local))];
	const diffs: ConflictFieldDiff[] = [];
	for (const key of keys) {
		if (local[key] === server[key]) continue;
		diffs.push({
			key,
			label: labels[key] ?? key,
			local: displayValue(local[key]),
			server: displayValue(server[key])
		});
	}
	return diffs;
}
