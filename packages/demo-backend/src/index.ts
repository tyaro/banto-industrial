/**
 * ブラウザで触れるデモ（GitHub Pages）の模擬サーバーの共通部分
 * （設計 docs/demo-pages-design.md §3.1・§3.3）。
 *
 * デモのビルド（`VITE_BANTO_DEMO=1`）のときだけ、各アプリの `setup.ts` が起動の最初に
 * 動的に import し、`installDemoBackend()` で `fetch`・`WebSocket`・`EventSource` を
 * 差し替える。画面と banto の provider から見ると、本物のサーバーと話しているのと同じに
 * なる（各 `*Admin.ts` と `@banto/admin-core` に手を入れない）。
 *
 * 本物の API に出ないことは、観測ではなくこの振り分けの制約で守る:
 *
 * - `/api/` で始まるもの（同じオリジンでも別のオリジンでも）は、口があれば模擬が答え、
 *   無ければ `501` と `{ "error": "demo_unsupported" }`。元の `fetch` を呼ばない。
 * - 元の `fetch` へ渡すのは、同じオリジンでベースパスの下の静的な資産（拡張子で判定）
 *   だけ。それ以外の宛先は `TypeError` で失敗する。
 * - `WebSocket`・`EventSource` は本物の接続を一度も開かない。口の無いパスは開いた直後に
 *   閉じる。
 *
 * 状態はメモリだけに持つ（このパッケージは状態を持たず、口ごとの状態は各アプリの
 * `src/lib/demo/` の側）。localStorage・sessionStorage は使わない。口ごとの応答は
 * 各アプリの `src/lib/demo/` に置き、型は各 `*Admin.ts`・`@banto/admin-core` から取る。
 */
export {
	classify,
	isApiPath,
	matchPath,
	normalizeBasePath,
	STATIC_ASSET_EXTENSIONS,
	type Destination,
	type PathPattern
} from './routing';
export {
	createDemoFetch,
	DEMO_UNSUPPORTED_BODY,
	idleEventStream,
	json,
	noContent,
	resolveInput,
	unsupported,
	type DemoFetchOptions,
	type DemoHttpHandler,
	type DemoHttpRoute,
	type DemoRequest
} from './http';
export {
	createDemoEventSourceClass,
	createDemoWebSocketClass,
	type DemoEventSourceConnection,
	type DemoEventSourceRoute,
	type DemoSocketConnection,
	type DemoSocketRoute
} from './sockets';
export { DEMO_BANNER_ID, mountDemoBanner } from './banner';
export {
	installDemoBackend,
	type DemoBackendHandle,
	type DemoGlobals,
	type DemoRoutes,
	type InstallDemoBackendOptions
} from './install';
