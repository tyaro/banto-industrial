import { guardCategory } from '../categories';

/** 試運転モード中のみ可視（`+layout.ts` の `visible.security` 参照）。それ以外は `guardCategory` が先頭の可視カテゴリへ弾く。 */
export async function load({ parent }) {
	const { categories } = await parent();
	guardCategory(categories, 'security');
}
