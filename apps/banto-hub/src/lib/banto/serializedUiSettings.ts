/**
 * UI 設定の書き込みをキーごとに直列化する `UiSettingsProvider` の包み（#1147）。
 *
 * 上流の `createHttpUiSettings.set` は PUT を待たずに並べて送るため、先の PUT が遅れて
 * 届くと古い値が最後に残る。ここでは:
 * - キーごとに、送信中の PUT は 1 本だけ。送信中に来た値は「最後の 1 つ」だけ持ち、
 *   送信が終わったら（成功・失敗とも）それを送る。古い中間値は送らずに捨てる。
 * - 別のキーは並行してよい。
 * - セッションの境界: 書き込みを受け付けた時点のトークンを値と一緒に控え、実際に送る
 *   直前のトークンと違えば（ログアウト・別の利用者）送らずに捨てる。トークンが無ければ
 *   （どうせ 401 なので）送らない。
 * - `get` は、そのキーの書き込み（送信中の 1 本と控えている最後の値）がすべて済むまで待って
 *   から内側へ渡す（読み直しが古い値を返さないように）。待つ間に来た新しい書き込みも待つ。
 *   書き込みが失敗しても `get` は reject しない。書き込みが無ければ素通し。
 *
 * `set` の Promise: 送られて成功したとき、または捨てられたとき（中間値・トークン違い・
 * トークン無し）は resolve、実際に送って失敗したときだけ reject（呼び出し側は
 * best-effort で握りつぶす）。
 */
import type { UiSettingsProvider } from '@banto/admin-core';

interface Pending {
	value: string;
	token: string;
	settle: Array<{ resolve: () => void; reject: (reason: unknown) => void }>;
}

export function createSerializedUiSettings(
	inner: UiSettingsProvider,
	getToken: () => string | null | undefined
): UiSettingsProvider {
	/** 送信中のキー → 送信中に来た最後の書き込み（無ければ null）。 */
	const inflight = new Map<string, Pending | null>();
	/** 送信中のキー → その drain の Promise（`get` が待つ。drain は reject しない）。 */
	const drains = new Map<string, Promise<void>>();

	async function drain(key: string, first: Pending): Promise<void> {
		let item: Pending | null = first;
		while (item) {
			const current: Pending = item;
			if (getToken() !== current.token) {
				// 受け付けた後にセッションが替わった: 送らずに捨てる。
				current.settle.forEach((s) => s.resolve());
			} else {
				try {
					await inner.set(key, current.value);
					current.settle.forEach((s) => s.resolve());
				} catch (error) {
					current.settle.forEach((s) => s.reject(error));
				}
			}
			item = inflight.get(key) ?? null;
			inflight.set(key, null);
		}
		inflight.delete(key);
		drains.delete(key);
	}

	return {
		async get(key) {
			// 書き込みが残っている間は待つ。待つ間に新しい drain が始まれば、それも待つ。
			while (inflight.has(key)) {
				const d = drains.get(key);
				if (!d) break;
				await d.catch(() => {});
			}
			return inner.get(key);
		},
		set(key, value) {
			const token = getToken();
			if (!token) return Promise.resolve();
			return new Promise<void>((resolve, reject) => {
				const entry: Pending = { value, token, settle: [{ resolve, reject }] };
				if (!inflight.has(key)) {
					inflight.set(key, null);
					const d = drain(key, entry);
					// 送る前に捨てられて drain が同期で終わった場合は登録しない。
					if (inflight.has(key)) drains.set(key, d);
					return;
				}
				// 送信中: 控えていた中間値は捨てる（その呼び出しは捨てられたものとして解決）。
				const superseded = inflight.get(key);
				superseded?.settle.forEach((s) => s.resolve());
				inflight.set(key, entry);
			});
		}
	};
}
