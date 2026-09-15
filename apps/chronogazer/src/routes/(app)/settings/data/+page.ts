import { guardCategory } from '../categories';

/** admin かつ audit/backups いずれか利用可能のときだけ可視（`+layout.ts` の `visible.data` 参照）。それ以外は `guardCategory` が先頭の可視カテゴリへ弾く。 */
export async function load({ parent }) {
	const { categories } = await parent();
	guardCategory(categories, 'data');
}
