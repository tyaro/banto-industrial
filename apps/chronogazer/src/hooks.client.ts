/**
 * クライアントの最初に走る（アプリのモジュールの読み込みより前）。
 *
 * デモ（GitHub Pages、docs/demo-pages-design.md §3.2）のビルドでだけ、localStorage・
 * sessionStorage をタブの中のメモリに差し替える。`settings.svelte.ts` などは読み込みの
 * 時点で localStorage を読むので、`bantoReady` の中（`startDemo()`）では遅い。この
 * 最上位の await が済むまで、アプリのモジュールは読み込まれない。
 * `import.meta.env.VITE_BANTO_DEMO` はビルドの時点で定数なので、製品のビルドでは
 * この分岐ごと消える。
 */
if (import.meta.env.VITE_BANTO_DEMO === '1') {
	const { installDemoStorage } = await import('@banto-industrial/demo-backend');
	installDemoStorage();
}

export {};
