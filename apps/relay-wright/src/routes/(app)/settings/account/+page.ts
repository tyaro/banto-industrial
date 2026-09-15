import { guardCategory } from '../categories';

/** 常に可視だが、他の4カテゴリと構造を揃えるため `guardCategory` は呼んでおく。 */
export async function load({ parent }) {
	const { categories } = await parent();
	guardCategory(categories, 'account');
}
