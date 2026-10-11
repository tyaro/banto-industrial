// banto v4.0.0 の admin-template `navigationSettled.test.ts` をコピー（banto #326）。
// ローカルの差分は、負荷時のタイムアウトと次のテストへの漏れを防ぐ
// ウォームアップ・タイムアウト・afterEach での決着だけ（#559 と同じ型）。
/**
 * Issue #326 (#321): the layouts start `refreshAll()` only when no
 * SvelteKit navigation is in progress - including the first one, which
 * SvelteKit does not publish in `navigating`.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
	afterNavigate: [] as Array<(navigation: { shallow: boolean }) => void>,
	navigating: { to: null as object | null }
}));

vi.mock('$app/navigation', () => ({
	afterNavigate: (callback: (navigation: { shallow: boolean }) => void) =>
		state.afterNavigate.push(callback)
}));
vi.mock('$app/state', () => ({ navigating: state.navigating }));

// 直近の `loadModule()` の import。初回の動的 import が負荷の高い全体実行で 5 秒を
// 超えて時間切れになっても、孤立した import が次のテスト中に評価されないよう、
// afterEach で決着させる（docs/implementation-checklist.md §5、#559 と同じ型）。
// hook の既定の時間制限（10 秒）では保留中の import を待ち切れないことがあるので、
// afterEach にも 60 秒の制限を明示する。
let pendingImport: Promise<unknown> | undefined;

function loadModule() {
	const imported = import('./navigationSettled.svelte');
	pendingImport = imported;
	return imported;
}

describe('isNavigationSettled', { timeout: 30_000 }, () => {
	// 先にモジュールグラフを一度読み込んで変換結果を温めておく（各テストの
	// `vi.resetModules()` + 再 import が速くなる）。$app/* の vi.mock は巻き上げ
	// られ、モジュールは import 時に関数を定義するだけなので副作用はない。
	beforeAll(async () => {
		await import('./navigationSettled.svelte');
	}, 60_000);

	afterEach(async () => {
		await pendingImport?.catch(() => {});
		pendingImport = undefined;
	}, 60_000);

	beforeEach(() => {
		vi.resetModules();
		state.afterNavigate.length = 0;
		state.navigating.to = null;
	});

	it('is false until the first navigation has completed, although `navigating.to` is null', async () => {
		const { isNavigationSettled, trackFirstNavigation } = await loadModule();
		trackFirstNavigation();
		expect(isNavigationSettled()).toBe(false);

		// SvelteKit 3 runs the navigation callbacks for shallow routing too
		// (`goto(url, { shallow: true })`); that is not the first navigation.
		state.afterNavigate.forEach((callback) => callback({ shallow: true }));
		expect(isNavigationSettled()).toBe(false);

		state.afterNavigate.forEach((callback) => callback({ shallow: false }));
		expect(isNavigationSettled()).toBe(true);
	});

	it('is false while a later navigation is in flight, and true again once it ends', async () => {
		const { isNavigationSettled, trackFirstNavigation } = await loadModule();
		trackFirstNavigation();
		state.afterNavigate.forEach((callback) => callback({ shallow: false }));

		state.navigating.to = { url: new URL('http://localhost/items') };
		expect(isNavigationSettled()).toBe(false);

		state.navigating.to = null;
		expect(isNavigationSettled()).toBe(true);
	});
});
