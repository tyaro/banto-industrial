/**
 * 差し替えの `WebSocket`・`EventSource`（設計 §3.1）。どちらも本物の接続を一度も開かない。
 *
 * - 口のあるパス: 開いた（`open`）あと、模擬の側が {@link DemoSocketConnection} /
 *   {@link DemoEventSourceConnection} から message を送れる。
 * - 口の無いパス: 開いた直後に閉じる（WebSocket は `error` と `close`（1006）、
 *   EventSource は `error` で `CLOSED`）。本物の接続の失敗と同じ形にして、画面の
 *   再接続の処理をそのまま通す。
 *
 * パスはオリジンを問わず `pathname` で引く（どこへもつながないので、宛先の判定は要らない）。
 */
import { matchPath, type PathPattern } from './routing';

/** 模擬の側から見た 1 本の WebSocket。 */
export interface DemoSocketConnection {
	readonly url: URL;
	readonly params: Record<string, string>;
	readonly protocols: readonly string[];
	/** 画面へ message を送る（開いている間だけ届く）。 */
	send(data: string): void;
	/** 模擬の側から閉じる。 */
	close(code?: number, reason?: string): void;
	/** 画面が送った message を受ける。 */
	onMessage(listener: (data: string) => void): void;
	/** どちらかが閉じたとき。 */
	onClose(listener: (code: number, reason: string) => void): void;
}

export interface DemoSocketRoute {
	path: PathPattern;
	connect(connection: DemoSocketConnection): void;
}

/** 模擬の側から見た 1 本の EventSource。 */
export interface DemoEventSourceConnection {
	readonly url: URL;
	readonly params: Record<string, string>;
	/** 画面へ送る。`event` を省くと `message`。 */
	send(data: string, event?: string): void;
	close(): void;
	onClose(listener: () => void): void;
}

export interface DemoEventSourceRoute {
	path: PathPattern;
	connect(connection: DemoEventSourceConnection): void;
}

function findRoute<R extends { path: PathPattern }>(
	routes: readonly R[],
	pathname: string
): { route: R; params: Record<string, string> } | null {
	for (const route of routes) {
		const params = matchPath(route.path, pathname);
		if (params !== null) return { route, params };
	}
	return null;
}

/** 次の macrotask で動かす（本物の接続と同じく、作った直後の同期の処理の後に届く）。 */
function later(fn: () => void): void {
	setTimeout(fn, 0);
}

function closeEvent(code: number, reason: string, wasClean: boolean): Event {
	if (typeof CloseEvent === 'function') return new CloseEvent('close', { code, reason, wasClean });
	const event = new Event('close');
	Object.assign(event, { code, reason, wasClean });
	return event;
}

type Handler = ((this: unknown, ev: Event) => unknown) | null;

/** `on<type>` の手と `addEventListener` の両方へ配る。 */
function fire(target: EventTarget, event: Event): void {
	const handler = (target as unknown as Record<string, unknown>)[`on${event.type}`] as
		Handler | undefined;
	target.dispatchEvent(event);
	if (typeof handler === 'function') handler.call(target, event);
}

/** `routes` に従う `WebSocket` の代わりのクラスを作る。 */
export function createDemoWebSocketClass(
	routes: readonly DemoSocketRoute[],
	baseUrl: () => string
): typeof WebSocket {
	class DemoWebSocket extends EventTarget {
		static readonly CONNECTING = 0;
		static readonly OPEN = 1;
		static readonly CLOSING = 2;
		static readonly CLOSED = 3;
		readonly CONNECTING = 0;
		readonly OPEN = 1;
		readonly CLOSING = 2;
		readonly CLOSED = 3;

		readonly url: string;
		readyState = 0;
		protocol = '';
		extensions = '';
		bufferedAmount = 0;
		binaryType: BinaryType = 'blob';
		onopen: Handler = null;
		onmessage: Handler = null;
		onerror: Handler = null;
		onclose: Handler = null;

		#messageListeners: Array<(data: string) => void> = [];
		#closeListeners: Array<(code: number, reason: string) => void> = [];

		constructor(url: string | URL, protocols?: string | string[]) {
			super();
			const resolved = new URL(String(url), baseUrl());
			this.url = resolved.href;
			const list =
				protocols === undefined ? [] : Array.isArray(protocols) ? protocols : [protocols];
			const found = findRoute(routes, resolved.pathname);
			if (!found) {
				later(() => this.#finish(1006, '', false, true));
				return;
			}
			const connection: DemoSocketConnection = {
				url: resolved,
				params: found.params,
				protocols: list,
				send: (data) => {
					if (this.readyState !== 1) return;
					later(() => {
						if (this.readyState === 1) {
							fire(this, new MessageEvent('message', { data }));
						}
					});
				},
				close: (code = 1000, reason = '') => {
					if (this.readyState >= 2) return;
					this.readyState = 2;
					later(() => this.#finish(code, reason, true, false));
				},
				onMessage: (listener) => {
					this.#messageListeners.push(listener);
				},
				onClose: (listener) => {
					this.#closeListeners.push(listener);
				}
			};
			later(() => {
				if (this.readyState !== 0) return;
				this.readyState = 1;
				this.protocol = list[0] ?? '';
				// 模擬の受け口（onMessage）を先に登録してから open を配る。onopen の中で
				// 同期に send するクライアントの message を取りこぼさないため。
				found.route.connect(connection);
				if (this.readyState === 1) fire(this, new Event('open'));
			});
		}

		send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
			if (this.readyState === 0) {
				throw new DOMException('Still in CONNECTING state.', 'InvalidStateError');
			}
			if (this.readyState !== 1) return;
			const text = typeof data === 'string' ? data : '';
			later(() => {
				for (const listener of [...this.#messageListeners]) listener(text);
			});
		}

		close(code = 1000, reason = ''): void {
			if (this.readyState >= 2) return;
			this.readyState = 2;
			later(() => this.#finish(code, reason, true, false));
		}

		#finish(code: number, reason: string, wasClean: boolean, withError: boolean): void {
			if (this.readyState === 3) return;
			this.readyState = 3;
			if (withError) fire(this, new Event('error'));
			fire(this, closeEvent(code, reason, wasClean));
			for (const listener of this.#closeListeners) listener(code, reason);
		}
	}
	return DemoWebSocket as unknown as typeof WebSocket;
}

/** `routes` に従う `EventSource` の代わりのクラスを作る。 */
export function createDemoEventSourceClass(
	routes: readonly DemoEventSourceRoute[],
	baseUrl: () => string
): typeof EventSource {
	class DemoEventSource extends EventTarget {
		static readonly CONNECTING = 0;
		static readonly OPEN = 1;
		static readonly CLOSED = 2;
		readonly CONNECTING = 0;
		readonly OPEN = 1;
		readonly CLOSED = 2;

		readonly url: string;
		readonly withCredentials: boolean;
		readyState = 0;
		onopen: Handler = null;
		onmessage: Handler = null;
		onerror: Handler = null;

		#closeListeners: Array<() => void> = [];

		constructor(url: string | URL, init?: EventSourceInit) {
			super();
			const resolved = new URL(String(url), baseUrl());
			this.url = resolved.href;
			this.withCredentials = init?.withCredentials ?? false;
			const found = findRoute(routes, resolved.pathname);
			if (!found) {
				later(() => {
					if (this.readyState === 2) return;
					this.readyState = 2;
					fire(this, new Event('error'));
				});
				return;
			}
			const connection: DemoEventSourceConnection = {
				url: resolved,
				params: found.params,
				send: (data, event = 'message') => {
					if (this.readyState !== 1) return;
					later(() => {
						if (this.readyState === 1) fire(this, new MessageEvent(event, { data }));
					});
				},
				close: () => {
					if (this.readyState === 2) return;
					this.readyState = 2;
					later(() => {
						fire(this, new Event('error'));
						this.#notifyClosed();
					});
				},
				onClose: (listener) => {
					this.#closeListeners.push(listener);
				}
			};
			later(() => {
				if (this.readyState !== 0) return;
				this.readyState = 1;
				fire(this, new Event('open'));
				found.route.connect(connection);
			});
		}

		close(): void {
			if (this.readyState === 2) return;
			this.readyState = 2;
			this.#notifyClosed();
		}

		#notifyClosed(): void {
			const listeners = this.#closeListeners;
			this.#closeListeners = [];
			for (const listener of listeners) listener();
		}
	}
	return DemoEventSource as unknown as typeof EventSource;
}
