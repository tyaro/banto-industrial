/**
 * The app's toast store (banto #220 phase 3, 2026-10-08): one `@banto/ui`
 * store (`createToastStore()`, banto ADR-0018 §8 phase 2c) shared by
 * `@banto/ui`'s `ToastHost` (mounted in routes/+layout.svelte) and every
 * caller. Wired as the admin-core `Notifier` in src/lib/banto/setup.ts, so
 * success/error/info messages from the list/form composables (spec §3.4)
 * surface here. Auto-dismiss stays at the package default (4000ms).
 */
import { createToastStore } from '@banto/ui';

export const toastStore = createToastStore();
