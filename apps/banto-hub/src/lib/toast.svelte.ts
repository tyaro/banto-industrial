/**
 * relay-wright の同名ファイルから無改変で複製。トースト通知ストア
 * （Svelte 5 runes）。src/lib/banto/setup.ts の Notifier としてここに配線。
 */
import type { NotificationKind } from '@banto/admin-core';

export interface Toast {
	id: number;
	kind: NotificationKind;
	message: string;
}

const AUTO_DISMISS_MS = 4000;

class ToastStore {
	toasts: Toast[] = $state([]);
	#nextId = 1;

	push(kind: NotificationKind, message: string): void {
		const id = this.#nextId++;
		this.toasts = [...this.toasts, { id, kind, message }];
		setTimeout(() => this.dismiss(id), AUTO_DISMISS_MS);
	}

	dismiss(id: number): void {
		this.toasts = this.toasts.filter((toast) => toast.id !== id);
	}
}

export const toastStore = new ToastStore();
