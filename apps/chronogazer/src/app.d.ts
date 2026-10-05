// See https://svelte.dev/docs/kit/types#app.d.ts
declare global {
	namespace App {
		interface Error {
			message: string;
			/**
			 * banto v3.0.1（#321）の admin-template `app.d.ts` を写した。起動待ちの
			 * 延期（`#lib/banto/startupGate.ts`）だけが立てる印: 保護ルートが起動の
			 * 完了前に開かれた。ルートのレイアウトはこの印のあいだエラー画面ではなく
			 * スプラッシュを出す。
			 */
			startupPending?: boolean;
		}
		// interface Locals {}
		// interface PageData {}
		// interface PageState {}
		// interface Platform {}
	}
}

export {};
