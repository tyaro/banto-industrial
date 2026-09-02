/**
 * `groupWritableDefault.ts`（T19 S1-b UX-34）に対するユニットテスト。
 *
 * このリポジトリの vitest 実行環境（Node、engines `"node": ">=24"` -
 * `package.json` 参照）は `localStorage` グローバルを「実験的 webstorage」
 * として持つが、`--localstorage-file` を指定しない限り**アクセスした
 * だけで `SecurityError` を投げる**（`typeof localStorage` の評価すら
 * 例外になる - `groupWritableDefault.ts` の doc comment 参照）。そのため:
 *
 * - 「`localStorage` に触れない/触れても例外」経路は**素の環境のまま**
 *   テストする（フォールバック値 `true` を返すことの確認）。
 * - 実際の get/set 往復は `Object.defineProperty(globalThis, 'localStorage',
 *   ...)` で最小限のインメモリ実装に差し替えてからテストする
 *   （`jsdom`/`happy-dom` 環境を導入する変更はこのモジュール単体のために
 *   するには重い - H5 のテスト基盤方針「純関数のユニットテストのみを対象」
 *   に合わせ、テスト内で必要な分だけ差し替える）。
 */
import { afterEach, describe, expect, it } from 'vitest';
import { getGroupDefaultWritable, setGroupDefaultWritable } from './groupWritableDefault';

interface MockStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
	removeItem(key: string): void;
}

function installMockStorage(): MockStorage {
	const store = new Map<string, string>();
	const mock: MockStorage = {
		getItem: (key) => (store.has(key) ? store.get(key)! : null),
		setItem: (key, value) => {
			store.set(key, value);
		},
		removeItem: (key) => {
			store.delete(key);
		}
	};
	Object.defineProperty(globalThis, 'localStorage', {
		value: mock,
		configurable: true,
		writable: true
	});
	return mock;
}

function uninstallMockStorage(): void {
	delete (globalThis as unknown as Record<string, unknown>).localStorage;
}

afterEach(() => {
	uninstallMockStorage();
});

describe('getGroupDefaultWritable: localStorage にアクセスできない環境', () => {
	it('未定義/例外環境では常に true（design の全体方針「既定 ON」）を返す', () => {
		// このテストプロセスの `localStorage` はこの時点で未モック - モジュール
		// doc comment の通り Node の webstorage 実装が SecurityError を投げる。
		expect(getGroupDefaultWritable(1)).toBe(true);
	});

	it('setGroupDefaultWritable も例外を投げずに黙って諦める', () => {
		expect(() => setGroupDefaultWritable(1, false)).not.toThrow();
	});
});

describe('getGroupDefaultWritable/setGroupDefaultWritable: モック localStorage での往復', () => {
	it('未設定のグループは true（既定 ON）', () => {
		installMockStorage();
		expect(getGroupDefaultWritable(42)).toBe(true);
	});

	it('false を保存すると false が返る', () => {
		installMockStorage();
		setGroupDefaultWritable(42, false);
		expect(getGroupDefaultWritable(42)).toBe(false);
	});

	it('true を明示的に保存しても true のまま', () => {
		installMockStorage();
		setGroupDefaultWritable(42, false);
		setGroupDefaultWritable(42, true);
		expect(getGroupDefaultWritable(42)).toBe(true);
	});

	it('グループごとに独立して保持される', () => {
		installMockStorage();
		setGroupDefaultWritable(1, false);
		expect(getGroupDefaultWritable(1)).toBe(false);
		expect(getGroupDefaultWritable(2)).toBe(true);
	});

	it('不正な保存値（想定外の文字列）は既定 ON として扱う', () => {
		const mock = installMockStorage();
		mock.setItem('banto-hub.collectionGroup.7.defaultWritable', 'garbage');
		expect(getGroupDefaultWritable(7)).toBe(true);
	});
});
