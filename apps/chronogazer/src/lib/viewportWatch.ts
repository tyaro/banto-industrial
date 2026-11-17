/**
 * T19 S3-a（UX-43）: `window.matchMedia` によるブレークポイント監視を
 * 依存注入可能な形に切り出した純粋部分。`$state` を使わないので vitest で
 * そのままテストできる（`mobileNav.svelte.ts` がこれを呼び出して runes に
 * つなぐ）。
 */

/** `matchMedia` が返すオブジェクトの、ここで使う最小インターフェース（テストでモック可能）。 */
export interface MediaQueryListLike {
	matches: boolean;
	addEventListener(type: 'change', listener: (event: { matches: boolean }) => void): void;
	removeEventListener(type: 'change', listener: (event: { matches: boolean }) => void): void;
}

export type MatchMediaFn = (query: string) => MediaQueryListLike;

/** 実行環境の `window.matchMedia` を使う既定実装。無ければ `undefined`。 */
function resolveDefaultMatchMedia(): MatchMediaFn | undefined {
	if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return undefined;
	return (query: string) => window.matchMedia(query);
}

/**
 * `query` に一致するかどうかを購読する。即座に現在値で `onChange` を1回
 * 呼び、以後は変化のたびに呼ぶ。戻り値は購読解除関数。
 *
 * SSR や `matchMedia` 未実装環境（このアプリは SPA だが念のためのガード）
 * では `onChange(false)` を1回呼ぶだけで、何もしない購読解除関数を返す。
 */
export function watchMediaQuery(
	query: string,
	onChange: (matches: boolean) => void,
	matchMediaFn: MatchMediaFn | undefined = resolveDefaultMatchMedia()
): () => void {
	if (!matchMediaFn) {
		onChange(false);
		return () => {};
	}

	const mql = matchMediaFn(query);
	onChange(mql.matches);

	const listener = (event: { matches: boolean }): void => onChange(event.matches);
	mql.addEventListener('change', listener);

	return () => mql.removeEventListener('change', listener);
}
