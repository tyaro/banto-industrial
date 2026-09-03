/**
 * relay-wright の同名ファイルから複製（当初は無改変）。トースト通知ストア
 * （Svelte 5 runes）。src/lib/banto/setup.ts の Notifier としてここに配線。
 *
 * T19 S2-c2（2026-09-03、UX-40）で分岐: タグ削除の取り消しトースト用に、
 * `push` へ任意のアクションボタンと任意の表示時間を追加した。既存呼び出し
 * （`push(kind, message)`）はシグネチャ互換のまま動く。
 *
 * T19 S2-c2 レビュー対応（2026-09-03）: `push` が作成したトーストの `id` を
 * 返すようにした。呼び出し元が実行完了時に `dismiss(id)` して、猶予切れや
 * `flush()` の前倒し実行より前に取り消しボタンを消せるようにするため
 * （`(app)/tags/+page.svelte` の `scheduleTagDeletion` 参照）。既存の
 * 呼び出しはすべて戻り値を無視するだけなので互換性は保たれる。
 */
import type { NotificationKind } from '@banto/admin-core';

export interface ToastAction {
	label: string;
	onClick: () => void;
}

export interface Toast {
	id: number;
	kind: NotificationKind;
	message: string;
	action?: ToastAction;
}

export interface ToastPushOptions {
	/** ボタン付きのアクション。押されると `onClick` を呼んだ後、このトーストを dismiss する。 */
	action?: ToastAction;
	/** 自動で消えるまでの時間(ms)。省略時は `AUTO_DISMISS_MS`。 */
	durationMs?: number;
}

const AUTO_DISMISS_MS = 4000;

class ToastStore {
	toasts: Toast[] = $state([]);
	#nextId = 1;

	push(kind: NotificationKind, message: string, options?: ToastPushOptions): number {
		const id = this.#nextId++;
		const action: ToastAction | undefined = options?.action
			? {
					label: options.action.label,
					onClick: () => {
						options.action!.onClick();
						this.dismiss(id);
					}
				}
			: undefined;
		this.toasts = [...this.toasts, { id, kind, message, action }];
		setTimeout(() => this.dismiss(id), options?.durationMs ?? AUTO_DISMISS_MS);
		return id;
	}

	dismiss(id: number): void {
		this.toasts = this.toasts.filter((toast) => toast.id !== id);
	}
}

export const toastStore = new ToastStore();
