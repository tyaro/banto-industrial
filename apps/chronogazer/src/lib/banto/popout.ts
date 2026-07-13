/**
 * Tauri-only pop-out window orchestration (spec §5.3 v2: "ウィンドウ分離"
 * モード). Turns a dock panel into a REAL native `WebviewWindow` via the thin
 * `panel_open` Tauri command (apps/admin-template/src-tauri/src/lib.rs), and
 * notifies the dashboard when that window closes so the panel can be
 * restored to the dock (`banto://panel-closed`, emitted by that same
 * command's `WindowEvent::Destroyed` handler).
 *
 * Both exports are `isTauri()`-guarded and are never called in plain-browser
 * mode: the dashboard page only wires `onPopOut`/`listenPanelClosed` when
 * `isTauri()` is true (see its doc comment), so `@banto/dock-svelte`'s
 * pop-out button doesn't even render there in the first place (`DockHost`'s
 * `onPopOut` prop is left `undefined`). These guards are a second line of
 * defense, not the primary one.
 */
import { invoke } from '@tauri-apps/api/core'; // safe to import outside Tauri (see setup.ts) - only ever CALLED when isTauri()
import type { PanelContent } from '@banto/dock-svelte';
import { isTauri } from './setup';

/**
 * Ask src-tauri to open (or, if already open, just focus) a native window
 * showing `content` at `routes/panel/[id]`. No-op outside Tauri.
 */
export async function openPanelWindow(content: PanelContent): Promise<void> {
	if (!isTauri()) return;
	await invoke('panel_open', { id: content.id, title: content.title });
}

/**
 * Subscribe to a popped-out panel window's closure. Returns an unlisten
 * function. Outside Tauri this is a no-op: the listener is never actually
 * registered (`@tauri-apps/api/event`'s `listen()` talks to a Tauri IPC
 * channel that doesn't exist in a plain browser tab - same reasoning as
 * `packages/admin-core/src/events.ts`'s `createTauriEventProvider`, which is
 * why the dynamic import below mirrors its disposal-race handling: if the
 * caller unsubscribes before the async `listen()` resolves, tear the
 * just-registered listener down immediately instead of leaking it).
 */
export function listenPanelClosed(cb: (id: string) => void): () => void {
	if (!isTauri()) return () => {};

	let disposed = false;
	let unlisten: (() => void) | null = null;

	void import('@tauri-apps/api/event').then(({ listen }) => {
		if (disposed) return;
		void listen('banto://panel-closed', (e) => cb(e.payload as string)).then((fn) => {
			if (disposed) fn();
			else unlisten = fn;
		});
	});

	return () => {
		disposed = true;
		unlisten?.();
	};
}
