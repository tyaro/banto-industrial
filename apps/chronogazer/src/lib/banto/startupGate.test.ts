// banto v4.0.0 の admin-template `startupGate.test.ts` を無改変でコピー（banto #321、#325）。
/**
 * Startup gate (Issue #321): a route guard defers instead of waiting while
 * startup is still running, with an error the root layout recognises.
 */
import { isHttpError } from '@sveltejs/kit';
import { describe, expect, it, vi } from 'vitest';
import { deferUntilStarted, isStartupDeferral } from './startupGate';

describe('deferUntilStarted', () => {
	it('lets the guard run once startup has finished, without building the message', () => {
		const message = vi.fn(() => 'starting');
		expect(() => deferUntilStarted(true, message)).not.toThrow();
		expect(message).not.toHaveBeenCalled();
	});

	it('throws a 503 marked as the startup deferral while startup is running', () => {
		let thrown: unknown;
		try {
			deferUntilStarted(false, () => 'starting');
		} catch (err) {
			thrown = err;
		}
		expect(isHttpError(thrown, 503)).toBe(true);
		const body = (thrown as { body: App.Error }).body;
		// SvelteKit 3 puts the status into the error body too (`App.Error.status`).
		expect(body).toEqual({ status: 503, message: 'starting', startupPending: true });
		expect(isStartupDeferral(body)).toBe(true);
	});
});

describe('isStartupDeferral', () => {
	it('is false for no error and for every other error (e.g. the session-check 503)', () => {
		expect(isStartupDeferral(null)).toBe(false);
		expect(isStartupDeferral(undefined)).toBe(false);
		expect(isStartupDeferral({ status: 503, message: 'ログイン状態を確認できませんでした' })).toBe(
			false
		);
		expect(isStartupDeferral({ status: 503, message: 'x', startupPending: false })).toBe(false);
	});
});
