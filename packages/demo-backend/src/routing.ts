/**
 * 宛先の振り分けの規則（設計 docs/demo-pages-design.md §3.1）。純関数だけを置く。
 *
 * - `api`: パスが `/api` か `/api/` で始まる。オリジンは問わない（別のオリジンの
 *   `/api/` も模擬が答えるか 501 で、ネットワークには出さない）。
 * - `static`: 同じオリジンで、ベースパスの下の静的な資産（拡張子で判定）。これだけが
 *   元の `fetch` へ渡る。
 * - `blocked`: それ以外（別のオリジン・ベースパスの外・拡張子の無いパス・http(s) 以外）。
 */

export type Destination = 'api' | 'static' | 'blocked';

/** 元の `fetch` へ渡してよい静的な資産の拡張子（小文字）。 */
export const STATIC_ASSET_EXTENSIONS: readonly string[] = [
	'js',
	'mjs',
	'css',
	'map',
	'json',
	'webmanifest',
	'png',
	'jpg',
	'jpeg',
	'gif',
	'svg',
	'ico',
	'webp',
	'avif',
	'woff',
	'woff2',
	'ttf',
	'otf',
	'wasm'
];

/** `/api` か `/api/...` か。 */
export function isApiPath(pathname: string): boolean {
	return pathname === '/api' || pathname.startsWith('/api/');
}

/** `basePath` を `''` か `/a/b`（先頭 `/`・末尾 `/` 無し）に揃える。 */
export function normalizeBasePath(basePath: string): string {
	const trimmed = basePath.replace(/\/+$/, '');
	if (trimmed === '') return '';
	return trimmed.startsWith('/') ? trimmed : `/${trimmed}`;
}

function extensionOf(pathname: string): string | null {
	const last = pathname.slice(pathname.lastIndexOf('/') + 1);
	const dot = last.lastIndexOf('.');
	if (dot <= 0 || dot === last.length - 1) return null;
	return last.slice(dot + 1).toLowerCase();
}

/**
 * `url` をどこへ回すかを決める。`origin` はページのオリジン、`basePath` はアプリの
 * ベースパス（SvelteKit の `base`）。
 */
export function classify(url: URL, origin: string, basePath: string): Destination {
	if (url.protocol !== 'http:' && url.protocol !== 'https:') return 'blocked';
	if (isApiPath(url.pathname)) return 'api';
	if (url.origin !== origin) return 'blocked';
	const base = normalizeBasePath(basePath);
	if (!url.pathname.startsWith(`${base}/`)) return 'blocked';
	const ext = extensionOf(url.pathname);
	return ext !== null && STATIC_ASSET_EXTENSIONS.includes(ext) ? 'static' : 'blocked';
}

/**
 * 口のパスの型。文字列は `/api/tags/:id` のように `:名前` で 1 区切りを受ける
 * （区切りの中の `/` は受けない）。正規表現なら名前付きの組を `params` にする。
 */
export type PathPattern = string | RegExp;

/** `pattern` が `pathname` に合えば `params`、合わなければ `null`。 */
export function matchPath(pattern: PathPattern, pathname: string): Record<string, string> | null {
	if (typeof pattern !== 'string') {
		const m = pattern.exec(pathname);
		if (!m || m.index !== 0 || m[0].length !== pathname.length) return null;
		return { ...(m.groups ?? {}) };
	}
	const want = pattern.split('/');
	const got = pathname.split('/');
	if (want.length !== got.length) return null;
	const params: Record<string, string> = {};
	for (let i = 0; i < want.length; i++) {
		const w = want[i];
		const g = got[i];
		if (w.startsWith(':')) {
			if (g === '') return null;
			params[w.slice(1)] = decodeURIComponent(g);
		} else if (w !== g) {
			return null;
		}
	}
	return params;
}
