/**
 * デモ用のメモリだけの `Storage`（設計 docs/demo-pages-design.md §3.2）。
 *
 * Pages は `tyaro.github.io` のオリジンを banto・temp-scada のデモと共有する。既存の画面の
 * コード（テーマ・ドックの配置・見張りの一覧・`@banto/admin-core` のトークンなど）は
 * 既定のキーで localStorage・sessionStorage に書くので、デモのビルドでは両方をタブの中の
 * メモリに差し替え、ほかのデモの設定やセッションを拾わない・残さない。
 *
 * 画面のモジュールの読み込みの時点で読む・書くコードがある（`settings.svelte.ts` など）
 * ので、差し替えは各アプリの `src/hooks.client.ts` の最上位で、一番早く行う。
 */

/** `Storage` 互換のメモリだけの実装。値は文字列に直して持つ（本物と同じ）。 */
export function createMemoryStorage(): Storage {
	const map = new Map<string, string>();
	const storage = {
		get length(): number {
			return map.size;
		},
		key(index: number): string | null {
			return [...map.keys()][index] ?? null;
		},
		getItem(key: string): string | null {
			return map.get(String(key)) ?? null;
		},
		setItem(key: string, value: string): void {
			map.set(String(key), String(value));
		},
		removeItem(key: string): void {
			map.delete(String(key));
		},
		clear(): void {
			map.clear();
		}
	};
	return storage as Storage;
}

export interface DemoStorageTarget {
	localStorage?: Storage;
	sessionStorage?: Storage;
}

const STORAGE_INSTALLED = Symbol.for('banto-industrial.demo-backend.storage-installed');

/**
 * `localStorage`・`sessionStorage` をメモリだけの物に差し替える。2 回目は何もしない
 * （同じ物を返す）。差し替えを戻す口は試験用。
 */
export function installDemoStorage(target?: DemoStorageTarget): { uninstall(): void } {
	const t = target ?? (globalThis as unknown as DemoStorageTarget);
	const marked = t as unknown as Record<symbol, unknown>;
	if (marked[STORAGE_INSTALLED]) return { uninstall() {} };
	const originals = {
		localStorage: Object.getOwnPropertyDescriptor(t, 'localStorage'),
		sessionStorage: Object.getOwnPropertyDescriptor(t, 'sessionStorage')
	};
	for (const name of ['localStorage', 'sessionStorage'] as const) {
		Object.defineProperty(t, name, {
			value: createMemoryStorage(),
			configurable: true,
			enumerable: true,
			writable: false
		});
	}
	marked[STORAGE_INSTALLED] = true;
	return {
		uninstall() {
			for (const name of ['localStorage', 'sessionStorage'] as const) {
				const original = originals[name];
				if (original) Object.defineProperty(t, name, original);
				else delete (t as Record<string, unknown>)[name];
			}
			delete marked[STORAGE_INSTALLED];
		}
	};
}
