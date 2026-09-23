import { guardCategory } from '../categories';

/**
 * 全ロールに開いている（`+layout.ts` の `visible.collect` は常に true）。
 * それでも `guardCategory` を通すのは、他のカテゴリと同じ形を保つためと、
 * 可視条件が将来変わったときにこのルートだけ素通しになるのを防ぐため。
 */
export async function load({ parent }) {
	const { categories } = await parent();
	guardCategory(categories, 'collect');
}
