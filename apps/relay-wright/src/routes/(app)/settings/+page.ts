import { redirect } from '@sveltejs/kit';

/**
 * `/settings` 自体は何も描画せず、先頭の可視カテゴリ（`+layout.ts` が
 * 計算した `categories`）へ常に redirect する（#359 relay-wright 分）。外部
 * ブックマーク・ディープリンク互換のためにこの redirect は必須 - `/settings`
 * を直接開いても迷子にならない。banto-hub・chronogazer の同名ファイルと
 * 同じパターン。
 */
export async function load({ parent }) {
	const { categories } = await parent();
	const first = categories[0];
	if (first) redirect(307, first.path);
}
