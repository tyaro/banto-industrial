/**
 * `/api/v1/stream`（WebSocket）の模擬（設計 docs/demo-pages-design.md §3.3）。プロトコルは
 * サーバー `apps/banto-hub/core/src/stream.rs` と同じ形:
 *
 * - `subscribe`（`id`・`tags`（`*`・`接続.グループ.*`・具体名）・`ids`）→ 直後に 1 回のスナップショット
 *   （`op: "data"`）、以後はグループの周期（下限 500ms）で**変わった値だけ**を送る。
 * - `unsubscribe`・`ping`（→ `pong`）。形の悪い要求は `op: "error"`（`invalid_request`）。
 * - 登録が変わったら全接続へ `op: "config_changed"`（`revision`）。
 *
 * 値は `@banto-industrial/demo-backend` の `demoTagValue`（タグ ID と時刻だけで決まる純関数）。
 * タイマーは 1 接続に 1 つ（本物の評価タイマと同じ 250ms）で、接続が閉じたら必ず止める。
 */
import {
	demoSampleTime,
	demoTagValue,
	type DemoSocketConnection,
	type DemoSocketRoute
} from '@banto-industrial/demo-backend';
import type { HubRegistry } from './hubState';

/** 評価の間隔（`subscribe_core.rs` の `EVAL_TICK_MS`）。 */
const EVAL_TICK_MS = 250;

interface WireValue {
	tag: string;
	id: number;
	v: number | null;
	q: 'good';
	t: number;
}

interface Subscription {
	/** `*` が入っているか。 */
	all: boolean;
	/** `接続.グループ` のワイルドカード。 */
	groups: Set<string>;
	/** 具体名。 */
	exact: Set<string>;
	ids: Set<number>;
	/** タグごとに最後に送った値（`t` と `v`）。変わっていなければ送らない。 */
	sent: Map<number, { t: number; v: number | null }>;
}

type Subscribe = { op: 'subscribe'; id: number; tags?: unknown; ids?: unknown };

function parseSubscription(msg: Subscribe): Subscription | string {
	const sub: Subscription = {
		all: false,
		groups: new Set(),
		exact: new Set(),
		ids: new Set(),
		sent: new Map()
	};
	const tags = Array.isArray(msg.tags) ? msg.tags : [];
	const ids = Array.isArray(msg.ids) ? msg.ids : [];
	if (tags.length === 0 && ids.length === 0) return 'tags か ids が必要です';
	for (const raw of tags) {
		if (typeof raw !== 'string' || raw === '') return 'タグ名が正しくありません';
		if (raw === '*') sub.all = true;
		else if (raw.endsWith('.*')) {
			const prefix = raw.slice(0, -2);
			const segments = prefix.split('.');
			if (segments.length !== 2 || segments.some((s) => s === '')) {
				return `ワイルドカードは {connection}.{group}.* か * 単独のみ許可されています: ${raw}`;
			}
			sub.groups.add(prefix);
		} else if (raw.includes('*')) return `不正なワイルドカード指定です: ${raw}`;
		else sub.exact.add(raw);
	}
	for (const id of ids) if (typeof id === 'number') sub.ids.add(id);
	return sub;
}

function matches(sub: Subscription, externalName: string, tagId: number): boolean {
	if (sub.all || sub.exact.has(externalName) || sub.ids.has(tagId)) return true;
	const dot = externalName.lastIndexOf('.');
	return dot > 0 && sub.groups.has(externalName.slice(0, dot));
}

/** この購読の、いま送るべき値（`snapshot` ならすべて、そうでなければ変わったものだけ）。 */
function evaluate(
	registry: HubRegistry,
	sub: Subscription,
	nowMs: number,
	snapshot: boolean
): WireValue[] {
	const values: WireValue[] = [];
	for (const tag of registry.tags) {
		const group = registry.group(tag.collectionGroupId);
		const connection = group ? registry.connection(group.plcConnectionId) : undefined;
		if (!group || !connection) continue;
		if (!tag.enabled || !group.enabled || !connection.enabled) continue;
		const externalName = `${connection.name}.${group.name}.${tag.name}`;
		if (!matches(sub, externalName, tag.id)) continue;
		const t = demoSampleTime(nowMs, group.periodMs);
		const v = demoTagValue(
			{ id: tag.id, dataType: tag.dataType, decimals: tag.decimals, periodMs: group.periodMs },
			t
		);
		const last = sub.sent.get(tag.id);
		if (!snapshot && last && (last.t === t || last.v === v)) {
			if (last.t !== t) sub.sent.set(tag.id, { t, v: last.v });
			continue;
		}
		sub.sent.set(tag.id, { t, v });
		values.push({ tag: externalName, id: tag.id, v, q: 'good', t });
	}
	return values;
}

export function createHubSocketRoute(
	registry: HubRegistry,
	now: () => number = Date.now
): DemoSocketRoute {
	return {
		path: '/api/v1/stream',
		connect(connection: DemoSocketConnection) {
			const subs = new Map<number, Subscription>();
			let closed = false;
			const send = (payload: unknown): void => {
				if (!closed) connection.send(JSON.stringify(payload));
			};
			const tick = (): void => {
				const t = now();
				for (const [id, sub] of subs) {
					const values = evaluate(registry, sub, t, false);
					if (values.length > 0) send({ op: 'data', id, t, values });
				}
			};
			const timer = setInterval(tick, EVAL_TICK_MS);
			const stopChange = registry.onChange((revision) => send({ op: 'config_changed', revision }));
			const stop = (): void => {
				if (closed) return;
				closed = true;
				clearInterval(timer);
				stopChange();
				subs.clear();
			};
			connection.onClose(stop);
			connection.onMessage((data) => {
				let msg: Record<string, unknown>;
				try {
					const parsed: unknown = JSON.parse(data);
					if (typeof parsed !== 'object' || parsed === null) throw new Error('not an object');
					msg = parsed as Record<string, unknown>;
				} catch {
					send({ op: 'error', code: 'invalid_request', detail: 'JSON が正しくありません' });
					return;
				}
				const id = typeof msg.id === 'number' ? msg.id : null;
				switch (msg.op) {
					case 'ping':
						send({ op: 'pong' });
						return;
					case 'unsubscribe':
						if (id !== null) subs.delete(id);
						return;
					case 'subscribe': {
						if (id === null) {
							send({ op: 'error', code: 'invalid_request', detail: 'id が必要です' });
							return;
						}
						const sub = parseSubscription(msg as Subscribe);
						if (typeof sub === 'string') {
							send({ op: 'error', id, code: 'invalid_request', detail: sub });
							return;
						}
						subs.set(id, sub);
						const t = now();
						// 値が 0 件でも、最初の `data` は送る（画面が「最初のスナップショット」を待つ）。
						send({ op: 'data', id, t, values: evaluate(registry, sub, t, true) });
						return;
					}
					default:
						send({ op: 'error', code: 'invalid_request', detail: '未対応の op です' });
				}
			});
		}
	};
}
