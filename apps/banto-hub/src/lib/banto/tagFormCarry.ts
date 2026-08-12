/**
 * T18-2c（docs/banto-hub-t18-design.md「T18-2c 登録後分岐と親引継ぎ」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-2「作成後は『登録して次へ』と
 * 『登録して閉じる』を分け、連続作業では親設定と明示選択した共通値を
 * 保持する」）: タグ単票 create Drawer の「登録して次へ」が使う、依存ゼロの
 * 純関数。`tagFormLayout.ts`/`tagFormNumeric.ts` と同じ方針 -
 * `+page.svelte` 側の `FormState` を直接 import せず、構造的部分型
 * （`name`/`address` の2フィールドだけを要求するジェネリック制約）で
 * 受け取ることで、ページ側の型に依存せずユニットテストできるようにする。
 *
 * 設計判断（§9.4 TAG-UX-2 の「明示選択した共通値」に対応する具体的な
 * チェックボックス UI 等の指定が設計書側に無いため、実装指示に明記された
 * 既定へフォールバックする）: **名前・アドレスの2フィールドだけをクリアし、
 * それ以外（タグ種別・収集グループ＝「親設定」を含む）は直前の入力を
 * そのまま引き継ぐ**。これにより「親設定は常に保持」も「その他の共通値
 * （データ型・単位・スケーリング・しきい値・有効/retain・書き込み許可等）
 * は直前の入力を既定として保持」も同時に満たす（`FormState` のフィールドは
 * この2つ以外すべてどちらか一方の分類に属するため）。
 */

/** `carryFormForNext` が要求する最小の形 - `name`/`address` を持つ文字列フィールド。 */
export interface CarryableTagForm {
	name: string;
	address: string;
}

/**
 * 「登録して次へ」用の次フォーム値を組み立てる。`previous`（直前に保存した
 * フォームの値そのもの、成功後もまだ `toInput` 送信前の生の文字列群）から
 * `name`/`address` だけを空文字列にし、他フィールドは型 `T` のまま丸ごと
 * 引き継ぐ。ジェネリックにしているのは、呼び出し元（`+page.svelte` の
 * ページ内 `FormState`）の具体的な型を保ったまま返したいため -
 * `CarryableTagForm` へ型を落とすと呼び出し側で再キャストが必要になる。
 */
export function carryFormForNext<T extends CarryableTagForm>(previous: T): T {
	return { ...previous, name: '', address: '' };
}
