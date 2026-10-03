import { guardCategory } from '../categories';

/** サーバーが試運転モード（未ロックダウン）のときだけ可視（`+layout.ts` の `visible.security`、セッションの種別では決めない）。それ以外は `guardCategory` が先頭の可視カテゴリへ弾く。 */
export async function load({ parent }) {
	const { categories } = await parent();
	guardCategory(categories, 'security');
}
