/**
 * Hub のデモ（GitHub Pages）の入口（設計 docs/demo-pages-design.md §3.1・§3.2）。
 *
 * `setup.ts` が `import.meta.env.VITE_BANTO_DEMO === '1'` のときだけ動的に import し、
 * {@link startDemo} が終わってから通常の provider を作る。製品のビルドにはこのディレクトリの
 * コードが入らない（`setup.ts` の分岐ごと消える）。
 *
 * 失敗したら（`?demo-fail-init=1` の試験の口を含む）、`setup.ts` は通常の provider を
 * 作らずに `bantoReady` を reject し、root layout が「デモを始められませんでした」を出す。
 */
import { resolve } from '$app/paths';
import { installDemoBackend, mountDemoBanner } from '@banto-industrial/demo-backend';
import { createBantoRoutes, resetDemoToken } from './bantoRoutes';
import { createHubRoutes } from './hubRoutes';

/** デモ専用のトークンのキー（既定の `banto.auth.token` は使わない、設計 §3.2）。 */
export const DEMO_TOKEN_STORAGE_KEY = 'banto-industrial.demo.hub.token';

/** 模擬の初期化を失敗させる試験の口（`e2e/tests-demo-pages/`）。デモのビルドにだけある。 */
export const DEMO_FAIL_INIT_PARAM = 'demo-fail-init';

/** 画面の上の帯の文言。 */
export const DEMO_BANNER_TEXT = 'デモ版 — 機器にはつながりません。読み直すと元に戻ります';

export interface DemoStart {
	tokenStorageKey: string;
}

export async function startDemo(): Promise<DemoStart> {
	// 差し替える**前**に失敗させる: これで通常の provider が動いてしまえば、本物の `/api` への
	// 要求がネットワークに出て、E2E の「送信 0 件」で見える。
	if (new URLSearchParams(location.search).has(DEMO_FAIL_INIT_PARAM)) {
		throw new Error('demo: 模擬の初期化を失敗させました（試験の口）');
	}
	installDemoBackend(
		{ http: [...createBantoRoutes(), ...createHubRoutes()] },
		{
			// SvelteKit 3 は `base` を出さないので、根の解決（`<base>/`）から取る。
			basePath: resolve('/'),
			onUnsupported: (method, url) =>
				console.warn(`demo: 模擬に口がありません: ${method} ${url.pathname}`)
		}
	);
	resetDemoToken(DEMO_TOKEN_STORAGE_KEY);
	mountDemoBanner(DEMO_BANNER_TEXT);
	return { tokenStorageKey: DEMO_TOKEN_STORAGE_KEY };
}
