// banto v3.0.0（タグ v3.0.0 = f0dcece）の admin-template
// `apps/admin-template/src/lib/banto/startupState.svelte.ts` からコピー（I2c、banto #286）。
// chronogazer 固有の差: なし（本文は無改変）。
/**
 * Reactive startup state for the splash screen (Issue #286): "connecting"
 * vs. "cannot reach the server" plus the user's retry action. Written by
 * setup.ts's `bantoReady`, read by `StartupSplash.svelte`.
 */
import type { StartupStatus } from './startup';

let status = $state<StartupStatus>('connecting');
let pendingRetry: (() => void) | null = null;

export const startupState = {
	get status(): StartupStatus {
		return status;
	}
};

export function setStartupStatus(next: StartupStatus): void {
	status = next;
}

/** Called by the startup loop: resolves on the next {@link retryStartup}. */
export function waitForStartupRetry(): Promise<void> {
	return new Promise((resolve) => {
		pendingRetry = resolve;
	});
}

/** User pressed "retry": flips back to "connecting" and lets the loop probe again. */
export function retryStartup(): void {
	const resolve = pendingRetry;
	pendingRetry = null;
	if (resolve) {
		status = 'connecting';
		resolve();
	}
}
