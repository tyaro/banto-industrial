/**
 * `/api/ui-settings/:key` の模擬（banto v8.0.0 の wire、#786）。このタブのメモリだけに持つ
 * （読み直すと消える）。両アプリ共通（口は banto の標準）。
 *
 * - `GET` は `{ value, revision }`。無ければ `value: null`・`revision: "0"`。
 * - `PUT` の本文は `{ value, ifRevision? }`、応答は `200 { revision }`。`ifRevision` が今の
 *   revision と違えば `409`（`kind: "conflict"`、`current` に今の `{ value, revision }`）。
 * - `DELETE ?ifRevision=` は `200 { revision: "0" }`。条件は PUT と同じ。
 * - revision は書き込みのたびに変わる不透明な文字列（本物は乱数。ここは連番）。
 */
import { json, type DemoHttpRoute } from './http';

/** 値が無いことを表す revision。 */
export const DEMO_UI_SETTING_ABSENT_REVISION = '0';

interface Entry {
	value: string;
	revision: string;
}

export function createUiSettingsRoutes(): DemoHttpRoute[] {
	const store = new Map<string, Entry>();
	let counter = 0;
	const snapshot = (key: string) => {
		const entry = store.get(key);
		return entry ?? { value: null, revision: DEMO_UI_SETTING_ABSENT_REVISION };
	};
	const conflict = (key: string): Response =>
		json(
			{
				kind: 'conflict',
				message: '設定が別の場所で更新されています',
				current: snapshot(key)
			},
			{ status: 409 }
		);
	const mismatched = (key: string, ifRevision: unknown): boolean =>
		typeof ifRevision === 'string' && snapshot(key).revision !== ifRevision;

	return [
		{
			method: 'GET',
			path: '/api/ui-settings/:key',
			handle: (req) => json(snapshot(req.params.key))
		},
		{
			method: 'PUT',
			path: '/api/ui-settings/:key',
			handle: async (req) => {
				const key = req.params.key;
				const body = await req.json<{ value?: unknown; ifRevision?: unknown }>();
				if (typeof body.value !== 'string') {
					return json({ kind: 'validation', message: 'value は文字列です' }, { status: 422 });
				}
				if (mismatched(key, body.ifRevision)) return conflict(key);
				counter += 1;
				const revision = String(counter);
				store.set(key, { value: body.value, revision });
				return json({ revision });
			}
		},
		{
			method: 'DELETE',
			path: '/api/ui-settings/:key',
			handle: (req) => {
				const key = req.params.key;
				if (mismatched(key, req.url.searchParams.get('ifRevision') ?? undefined)) {
					return conflict(key);
				}
				store.delete(key);
				return json({ revision: DEMO_UI_SETTING_ABSENT_REVISION });
			}
		}
	];
}
