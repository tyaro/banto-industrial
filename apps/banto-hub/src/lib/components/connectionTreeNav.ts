/**
 * #1179 PR2a: `ConnectionTree` が「フォーカスの移動に選択が付いてきた」のか
 * 「選んだ」のかを見分けるための純関数。
 *
 * banto の `BantoTree` は単一選択ではフォーカスに選択が付いてくる（上下・
 * Home・End・左右で `onSelectionChange` が呼ばれる）。狭幅で退避パネルを
 * 閉じる契機は「選んだ」ときだけにしたいので、直前の入力がナビゲーションの
 * キーだったかを `ConnectionTree` が記録し、`onselect` の第 2 引数
 * `{ navigation }` で伝える。BantoTree の DOM やクラスには依存しない
 * （キーの名前だけを見る）。
 */

/** フォーカスを動かすキー。これで起きた選択は `navigation: true` にする。 */
const NAVIGATION_KEYS: ReadonlySet<string> = new Set([
	'ArrowUp',
	'ArrowDown',
	'ArrowLeft',
	'ArrowRight',
	'Home',
	'End',
	'PageUp',
	'PageDown'
]);

/** `KeyboardEvent.key` が、フォーカス移動（＝選択が付いてくる）のキーか。 */
export function isNavigationKey(key: string): boolean {
	return NAVIGATION_KEYS.has(key);
}
