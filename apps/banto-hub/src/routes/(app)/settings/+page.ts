import { redirect } from '@sveltejs/kit';

/**
 * `/settings` 自体は何も描画せず、先頭の可視カテゴリ（`+layout.ts` が
 * 計算した `categories`）へ常に redirect する（#359 段階2）。外部
 * ブックマーク・ディープリンク互換のためにこの redirect は必須 -
 * `/settings` を直接開いても迷子にならない。ルート `routes/+page.ts` が
 * `/` を `/status` へ redirect するのと同じパターン。
 */
export async function load({ parent }) {
	const { categories } = await parent();
	const first = categories[0];
	if (first) redirect(307, first.path);
}
