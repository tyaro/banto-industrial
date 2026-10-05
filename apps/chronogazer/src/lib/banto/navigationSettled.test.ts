// banto v4.0.0 の admin-template `navigationSettled.test.ts` を無改変でコピー（banto #326）。
/**
 * Issue #326 (#321): the layouts start `refreshAll()` only when no
 * SvelteKit navigation is in progress - including the first one, which
 * SvelteKit does not publish in `navigating`.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
	afterNavigate: [] as Array<(navigation: { shallow: boolean }) => void>,
	navigating: { to: null as object | null }
}));

vi.mock('$app/navigation', () => ({
	afterNavigate: (callback: (navigation: { shallow: boolean }) => void) =>
		state.afterNavigate.push(callback)
}));
vi.mock('$app/state', () => ({ navigating: state.navigating }));

describe('isNavigationSettled', () => {
	beforeEach(() => {
		vi.resetModules();
		state.afterNavigate.length = 0;
		state.navigating.to = null;
	});

	it('is false until the first navigation has completed, although `navigating.to` is null', async () => {
		const { isNavigationSettled, trackFirstNavigation } =
			await import('./navigationSettled.svelte');
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
		const { isNavigationSettled, trackFirstNavigation } =
			await import('./navigationSettled.svelte');
		trackFirstNavigation();
		state.afterNavigate.forEach((callback) => callback({ shallow: false }));

		state.navigating.to = { url: new URL('http://localhost/items') };
		expect(isNavigationSettled()).toBe(false);

		state.navigating.to = null;
		expect(isNavigationSettled()).toBe(true);
	});
});
