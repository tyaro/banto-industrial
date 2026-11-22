/**
 * `fetch`・`WebSocket`・`EventSource` をまとめて差し替える口（設計 §3.1）。
 */
import { createDemoFetch, type DemoHttpRoute } from './http';
import { normalizeBasePath } from './routing';
import {
	createDemoEventSourceClass,
	createDemoWebSocketClass,
	type DemoEventSourceRoute,
	type DemoSocketRoute
} from './sockets';

export interface DemoRoutes {
	http: readonly DemoHttpRoute[];
	sockets?: readonly DemoSocketRoute[];
	eventSources?: readonly DemoEventSourceRoute[];
}

export interface InstallDemoBackendOptions {
	/** アプリのベースパス（SvelteKit の `base`）。静的な資産はこの下だけを通す。 */
	basePath: string;
	/** 差し替える先（既定は `globalThis`。試験で別の物を渡す）。 */
	target?: DemoGlobals;
	/** ページのオリジン（既定は `target.location.origin`）。 */
	origin?: string;
	/** 口の無い `/api/` に 501 を返したとき。 */
	onUnsupported?: (method: string, url: URL) => void;
}

/** 差し替える global の形。 */
export interface DemoGlobals {
	fetch: typeof fetch;
	WebSocket?: typeof WebSocket;
	EventSource?: typeof EventSource;
	location?: { origin: string };
}

export interface DemoBackendHandle {
	/** 差し替えを戻す（試験用。デモの画面では戻さない）。 */
	uninstall(): void;
}

const INSTALLED = Symbol.for('banto-industrial.demo-backend.installed');

/**
 * 差し替える。2 回目は投げる（二重に包むと、元の `fetch` が模擬の `fetch` になる）。
 * 状態は持たない（口の状態は `routes` の側のメモリだけ）。
 */
export function installDemoBackend(
	routes: DemoRoutes,
	options: InstallDemoBackendOptions
): DemoBackendHandle {
	const target = options.target ?? (globalThis as unknown as DemoGlobals);
	const marked = target as unknown as Record<symbol, unknown>;
	if (marked[INSTALLED]) throw new Error('demo-backend は既に入っています');
	const origin = options.origin ?? target.location?.origin;
	if (!origin) throw new Error('demo-backend: ページのオリジンが分かりません');
	const basePath = normalizeBasePath(options.basePath);
	const baseUrl = `${origin}${basePath}/`;

	const saved = {
		fetch: target.fetch,
		WebSocket: target.WebSocket,
		EventSource: target.EventSource
	};
	const originalFetch = saved.fetch.bind(target as unknown as typeof globalThis);

	target.fetch = createDemoFetch({
		routes: routes.http,
		originalFetch,
		origin,
		basePath,
		baseUrl,
		onUnsupported: options.onUnsupported
	});
	target.WebSocket = createDemoWebSocketClass(routes.sockets ?? [], () => baseUrl);
	target.EventSource = createDemoEventSourceClass(routes.eventSources ?? [], () => baseUrl);
	marked[INSTALLED] = true;

	return {
		uninstall() {
			target.fetch = saved.fetch;
			target.WebSocket = saved.WebSocket;
			target.EventSource = saved.EventSource;
			delete marked[INSTALLED];
		}
	};
}
