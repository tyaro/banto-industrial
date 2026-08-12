/**
 * T18-3a（docs/banto-hub-t18-design.md「T18-3a タグ複製」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-D 前半「『このタグを複製』、
 * 型/単位/スケーリング/しきい値を引継ぎ名前とアドレスのみ変更する」）:
 * タグ単票の「複製」Drawer が使う、依存ゼロの純関数。`tagFormCarry.ts`/
 * `tagFormLayout.ts` と同じ方針 - `+page.svelte` 側の `FormState` を直接
 * import せず、構造的部分型（`name`/`address` の2フィールドだけを要求する
 * ジェネリック制約）で受け取ることで、ページ側の型に依存せずユニット
 * テストできるようにする。
 *
 * `tagFormCarry.ts::carryFormForNext`（「登録して次へ」用、名前・アドレス
 * を空文字列にするだけ）とは異なり、複製は
 * - 保存前に何もタイプしなくても既存タグと重複しない名前が入っている
 *   （受け入れ「既存タグを上書きせず新規作成」を、保存前の時点で名前の
 *   重複という形で壊さないようにする最初の防波堤 - 最終的な一意性検証は
 *   既存どおりサーバー側 `createTag` が正）
 * - 複製元がどのタグだったか（`+page.svelte` 側が「複製元との差分」表示に
 *   使う）を呼び出し側で別途保持する
 * の2点が要る。後者は `FormState` 自体（複製元のタグを `formFromTag` に
 * 通した結果）と `diffFormRecords`（`tagConflictDiff.ts`、revision 競合の
 * 差分パネルが使っているのと同じ純関数）で足りるため、本ファイルに専用の
 * 差分ヘルパーは追加しない（+page.svelte 側で
 * `diffFormRecords(複製元フォーム, 複製後フォーム, FIELD_LABELS)` を
 * そのまま呼べる）。
 */

/** {@link buildDuplicateFormValues} が要求する最小の形 - `name`/`address` を持つ文字列フィールド。 */
export interface DuplicatableTagForm {
	name: string;
	address: string;
}

/**
 * 複製元タグの名前 `baseName` から、`existingNames` のどれとも重複しない
 * 複製名を組み立てる。基底名は `${baseName}_copy`、それが既に使われて
 * いれば `_copy2`・`_copy3`…と昇順に最小の未使用番号を探す（`_copy1` は
 * 使わない - 基底名そのものが「1個目の複製」を表す）。
 *
 * `existingNames` は大文字小文字を区別する完全一致で比較する - タグ名の
 * 一意性制約自体（サーバー側）に合わせ、ここで独自の正規化はしない。
 */
export function buildDuplicateName(baseName: string, existingNames: readonly string[]): string {
	const existing = new Set(existingNames);
	const base = `${baseName}_copy`;
	if (!existing.has(base)) return base;
	let n = 2;
	while (existing.has(`${baseName}_copy${n}`)) n += 1;
	return `${baseName}_copy${n}`;
}

/**
 * 複製後フォームの初期値を組み立てる。`source`（複製元タグを
 * `formFromTag` 等でフォーム形へ変換した値、id/revision は含まない）から
 * - `name`: {@link buildDuplicateName} で衝突しない複製名に差し替える。
 * - `address`: 空文字列にする（新アドレスはユーザーが入力する - PLC
 *   アドレスは通常タグ間で一意に対応するため、複製元のアドレスをそのまま
 *   引き継ぐと大抵の場合そのまま二重登録エラーになるか、意図せず同じ
 *   デバイスを指す設定ミスになる）。
 * - それ以外のフィールド（型/単位/スケーリング/しきい値/有効/書き込み許可/
 *   タグ種別/式/retain/収集グループ等）はすべてそのまま引き継ぐ。
 *
 * ジェネリックにしているのは `carryFormForNext` と同じ理由 - 呼び出し元
 * （`+page.svelte` の `FormState`）の具体的な型を保ったまま返したいため。
 */
export function buildDuplicateFormValues<T extends DuplicatableTagForm>(
	source: T,
	existingNames: readonly string[]
): T {
	return {
		...source,
		name: buildDuplicateName(source.name, existingNames),
		address: ''
	};
}
