import { guardCategory } from '../categories';

/** admin 限定（`+layout.ts` の `visible.hub` 参照）。非 admin は `guardCategory` が先頭の可視カテゴリへ弾く。 */
export async function load({ parent }) {
	const { categories } = await parent();
	guardCategory(categories, 'hub');
}
