/**
 * CSV 出力・日報に出すペンの選択（#1207・#1231 PR 5a、trend-design.md §6）の純関数。
 *
 * トレンドの履歴の口は 1 表示グループ 16 ペンまで読めるが、期間 CSV と日報は 8 本のまま
 * （`EXPORT_MAX_TAGS`・`DAILY_REPORT_MAX_TAGS`）。9 本以上のペンを持つグループを出すときは、
 * 先頭の 8 本に黙って絞らず、ペンのチェックの一覧から選ばせる。
 *
 * - 選んだ状態は画面の中だけで持ち、URL には入れない。持つのは「選んだタグ ID」か `null`
 *   （= 既定の選択）。`null` のときは、グループの先頭から上限までを選んだことにする。
 * - 選べるのはグループのペンだけ（グループが変わって消えたタグは落ちる）。順はグループの順。
 * - 上限を超えては選べない（超えるチェックは無視する）。
 */

/** CSV・日報の案内文（trend-design.md §6 の文言）。 */
export const PEN_SELECTION_NOTICE = 'CSV・日報は 8 本までです。出すペンを選んでください';

/** 上限を超えるペンを持つ（= 選択の一覧を見せる）か。 */
export function needsPenSelection(penCount: number, limit: number): boolean {
	return penCount > limit;
}

/** 既定の選択: グループの先頭から上限まで。 */
export function defaultPenSelection(tagIds: readonly number[], limit: number): number[] {
	return tagIds.slice(0, Math.max(0, limit));
}

/**
 * 実際に出すタグ（グループの順・上限まで・重複なし）。`chosen` が `null`（まだ選んでいない）
 * なら既定。選んだ中にグループに無いタグがあれば落とす。上限以下のグループは、選択に
 * 関係なく全部（一覧を出さないので今までと同じ）。
 */
export function effectivePenSelection(
	tagIds: readonly number[],
	chosen: readonly number[] | null,
	limit: number
): number[] {
	if (!needsPenSelection(tagIds.length, limit) || chosen === null) {
		return defaultPenSelection(tagIds, limit);
	}
	const picked = new Set(chosen);
	return [...new Set(tagIds)].filter((id) => picked.has(id)).slice(0, Math.max(0, limit));
}

/** チェックを切り替えた後の選択。上限に達しているときの新しいチェックは無視する。 */
export function togglePenSelection(
	tagIds: readonly number[],
	current: readonly number[],
	tagId: number,
	limit: number
): number[] {
	const now = effectivePenSelection(tagIds, current, limit);
	if (now.includes(tagId)) return now.filter((id) => id !== tagId);
	if (!tagIds.includes(tagId) || now.length >= limit) return now;
	const next = new Set([...now, tagId]);
	return tagIds.filter((id) => next.has(id));
}

/** このペンのチェックを入れられないか（上限に達していて、まだ選んでいないペン）。 */
export function isPenCheckDisabled(
	selected: readonly number[],
	tagId: number,
	limit: number
): boolean {
	return !selected.includes(tagId) && selected.length >= limit;
}

/** 「選択中 3 / 8 本」の表示。 */
export function penSelectionCountLabel(selected: number, limit: number): string {
	return `選択中 ${selected} / ${limit} 本`;
}
