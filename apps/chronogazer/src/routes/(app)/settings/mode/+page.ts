import { guardCategory } from '../categories';

/** Tauri のデスクトップの窓（設定モード）かつ admin のときだけ可視（`+layout.ts` の `visible.mode` 参照）。それ以外は `guardCategory` が先頭の可視カテゴリへ弾く。 */
export async function load({ parent }) {
	const { categories } = await parent();
	guardCategory(categories, 'mode');
}
