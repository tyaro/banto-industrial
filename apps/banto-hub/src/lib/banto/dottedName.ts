/**
 * 接続名・グループ名に `.` を使えない規則（2026-11-12 オーナー決定、#635 の続き）の
 * クライアント側の事前検査。サーバー側 `crates/banto-tags/src/support.rs::dotted_name_error`
 * のミラーで、文言を一字一句揃える（最終の判定はサーバー）。
 *
 * Hub の外部名は `接続.グループ.タグ` なので、接続名・グループ名に `.` が無ければ外部名は
 * 最初の 2 つの `.` で一意に分けられる（タグ名の `.` は引き続き使える）。
 *
 * `storedName` は編集中の行の今の名前（作成は省略）。名前を変えない更新は、この規則より
 * 前に作られた `.` を含む名前のままでも通す。比較・検査は前後の空白を除いた名前で行う。
 */
export type DottedNameLabel = '接続名' | 'グループ名';

export function dottedNameError(
	name: string,
	label: DottedNameLabel,
	storedName?: string
): string | null {
	const trimmed = name.trim();
	if (!trimmed.includes('.')) return null;
	if (storedName !== undefined && storedName.trim() === trimmed) return null;
	return `${label}に「.」は使えません（外部名「接続.グループ.タグ」の区切りに使うため）`;
}
