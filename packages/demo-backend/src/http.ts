/**
 * 差し替えの `fetch`（設計 §3.1）。`/api/` は模擬の口が答え、口が無ければ 501。
 * 元の `fetch` を呼ぶのは同じオリジンでベースパスの下の静的な資産だけで、ほかの宛先は
 * `TypeError` で失敗させる（本物の `fetch` のネットワークの失敗と同じ形）。
 */
import { classify, matchPath, type PathPattern } from './routing';

/** 口へ渡す要求。`Request` の写しで、本文は 1 回だけ読める。 */
export interface DemoRequest {
	method: string;
	url: URL;
	/** パスの `:名前`（または正規表現の名前付きの組）の値。 */
	params: Record<string, string>;
	headers: Headers;
	signal: AbortSignal;
	text(): Promise<string>;
	json<T = unknown>(): Promise<T>;
}

export type DemoHttpHandler = (request: DemoRequest) => Response | Promise<Response>;

export interface DemoHttpRoute {
	/** `GET`・`POST` など（大文字）。 */
	method: string;
	/** `/api/...` のパス（オリジン・クエリを含まない）。 */
	path: PathPattern;
	handle: DemoHttpHandler;
}

export interface DemoFetchOptions {
	routes: readonly DemoHttpRoute[];
	/** 静的な資産だけを渡す、差し替える前の `fetch`。 */
	originalFetch: typeof fetch;
	/** ページのオリジン（`location.origin`）。 */
	origin: string;
	/** アプリのベースパス（SvelteKit の `base`。無ければ `''`）。 */
	basePath: string;
	/** 相対パスの解決の基準（既定は `origin + basePath + '/'`）。 */
	baseUrl?: string;
	/** 口の無い `/api/` に 501 を返したときに呼ぶ（開発の手がかり用）。 */
	onUnsupported?: (method: string, url: URL) => void;
}

/** 模擬に口の無い `/api/` の応答の本文（設計 §3.4）。 */
export const DEMO_UNSUPPORTED_BODY = { error: 'demo_unsupported' } as const;

/** JSON の応答。 */
export function json(body: unknown, init: ResponseInit = {}): Response {
	const headers = new Headers(init.headers);
	if (!headers.has('content-type')) headers.set('content-type', 'application/json');
	return new Response(JSON.stringify(body), { ...init, status: init.status ?? 200, headers });
}

/** `204 No Content`。 */
export function noContent(): Response {
	return new Response(null, { status: 204 });
}

/** 口の無い `/api/` の応答（`501` と `{ "error": "demo_unsupported" }`）。 */
export function unsupported(): Response {
	return json(DEMO_UNSUPPORTED_BODY, { status: 501 });
}

/**
 * つないだまま何も送らない `text/event-stream` の応答（SSE の `/api/events`、設計 §3.3）。
 * 要求の `signal` が止められたら、本物の `fetch` と同じく本文の読み出しを AbortError で
 * 終える。
 */
export function idleEventStream(signal: AbortSignal): Response {
	const body = new ReadableStream<Uint8Array>({
		start(controller) {
			const stop = () => {
				try {
					controller.error(signal.reason ?? new DOMException('aborted', 'AbortError'));
				} catch {
					// 既に閉じている。
				}
			};
			if (signal.aborted) stop();
			else signal.addEventListener('abort', stop, { once: true });
		}
	});
	return new Response(body, {
		status: 200,
		headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' }
	});
}

function isRequest(input: unknown): input is Request {
	return typeof Request !== 'undefined' && input instanceof Request;
}

/** `fetch` の第 1 引数（文字列・`URL`・`Request`）を絶対の URL にする。 */
export function resolveInput(input: RequestInfo | URL, baseUrl: string): URL {
	if (isRequest(input)) return new URL(input.url);
	if (input instanceof URL) return new URL(input.href);
	return new URL(String(input), baseUrl);
}

function abortReason(signal: AbortSignal): unknown {
	return signal.reason ?? new DOMException('The operation was aborted.', 'AbortError');
}

/** 差し替えの `fetch` を作る。 */
export function createDemoFetch(options: DemoFetchOptions): typeof fetch {
	const baseUrl = options.baseUrl ?? `${options.origin}${options.basePath.replace(/\/+$/, '')}/`;
	const demoFetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
		const url = resolveInput(input, baseUrl);
		const destination = classify(url, options.origin, options.basePath);
		if (destination === 'static') return options.originalFetch(input, init);
		if (destination === 'blocked') {
			throw new TypeError(`demo: ${url.origin}${url.pathname} へは送りません（模擬の外）`);
		}

		const request = isRequest(input) ? new Request(input, init) : new Request(url.href, init);
		if (request.signal.aborted) throw abortReason(request.signal);
		const method = request.method.toUpperCase();
		for (const route of options.routes) {
			if (route.method.toUpperCase() !== method) continue;
			const params = matchPath(route.path, url.pathname);
			if (params === null) continue;
			const demoRequest: DemoRequest = {
				method,
				url,
				params,
				headers: request.headers,
				signal: request.signal,
				text: () => request.text(),
				json: async <T>() => JSON.parse(await request.text()) as T
			};
			try {
				return await route.handle(demoRequest);
			} catch (error) {
				if (request.signal.aborted) throw abortReason(request.signal);
				console.error('demo: 模擬の口が失敗しました', method, url.pathname, error);
				return json({ error: 'demo_internal' }, { status: 500 });
			}
		}
		options.onUnsupported?.(method, url);
		return unsupported();
	};
	return demoFetch as typeof fetch;
}
