// banto v3.0.0（タグ v3.0.0 = f0dcece）の admin-template
// `apps/admin-template/src/lib/banto/startup.ts` からコピー（I2c、banto #286）。
// chronogazer 固有の差: なし（本文は無改変）。
/**
 * Startup environment resolution (Issue #286). Pure (all effects injected) so
 * the Tauri / LAN-browser / intended-demo / transient-failure paths are unit
 * testable; setup.ts (the composition root) supplies the real dependencies.
 *
 * Deployment kind is decided explicitly, never inferred from a failed request:
 * - Tauri webview            -> `tauri`
 * - build marked as the demo -> `demo` (no probe; `VITE_BANTO_DEMO=1`)
 * - otherwise probe `/api/auth/check`:
 *     server      -> `server`
 *     none        -> `demo` (a static host definitively answered "no API here")
 *     unreachable -> NOT demo: retry a bounded number of times automatically,
 *                    then show a "cannot connect" state and wait for the
 *                    user's retry. Resolves only once a real answer arrives.
 */
import type { BackendProbe } from './environment';

export type StartupTarget = 'tauri' | 'server' | 'demo';

/** Automatic re-probes after the first failure before the "cannot connect" screen appears. */
export const STARTUP_AUTO_RETRIES = 2;
/** Pause between automatic re-probes. */
export const STARTUP_RETRY_DELAY_MS = 1500;

export type StartupStatus = 'connecting' | 'unreachable';

export interface StartupDeps {
	isTauri: () => boolean;
	isDemoBuild: () => boolean;
	probe: () => Promise<BackendProbe>;
	sleep: (ms: number) => Promise<void>;
	/** Reflects the state in the UI (splash text vs. cannot-connect + retry button). */
	setStatus: (status: StartupStatus) => void;
	/** Resolves when the user presses "retry". */
	waitForRetry: () => Promise<void>;
}

export async function resolveStartupTarget(deps: StartupDeps): Promise<StartupTarget> {
	if (deps.isTauri()) return 'tauri';
	if (deps.isDemoBuild()) return 'demo';
	for (;;) {
		deps.setStatus('connecting');
		for (let attempt = 0; attempt <= STARTUP_AUTO_RETRIES; attempt++) {
			const result = await deps.probe();
			if (result === 'server') return 'server';
			if (result === 'none') return 'demo';
			if (attempt < STARTUP_AUTO_RETRIES) await deps.sleep(STARTUP_RETRY_DELAY_MS);
		}
		deps.setStatus('unreachable');
		await deps.waitForRetry();
	}
}
