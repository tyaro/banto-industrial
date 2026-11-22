import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DemoSocketConnection } from '@banto-industrial/demo-backend';
import { HubRegistry, DemoValidationError, DemoRevisionConflictError } from './hubState';
import { createHubSocketRoute } from './hubStream';
import type { TagInput } from '#lib/banto/tagRegistryAdmin.js';

const NEW_TAG: TagInput = {
	name: 'added',
	collectionGroupId: 2,
	address: '40100',
	dataType: 'u16',
	decimals: 0,
	enabled: true
};

describe('HubRegistry（模擬の登録）', () => {
	it('初期のデータ: 接続 2 本・グループ 2 つ・タグ 20 本（数値・bit・文字列）', () => {
		const r = new HubRegistry();
		expect(r.connections).toHaveLength(2);
		expect(r.groups).toHaveLength(2);
		expect(r.tags).toHaveLength(20);
		const types = new Set(r.tags.map((t) => t.dataType));
		expect(types.has('bit') && types.has('f32') && types.has('string')).toBe(true);
	});

	it('登録・編集・削除が revision を進め、catalog に出る', () => {
		const r = new HubRegistry();
		const rev0 = r.revision;
		const created = r.createTag(NEW_TAG);
		expect(r.revision).toBe(rev0 + 1);
		expect(r.catalog().tags).toHaveLength(21);
		const updated = r.updateTag(created.id, { ...NEW_TAG, unit: 'rpm', expectedRevision: 1 });
		expect(updated.revision).toBe(2);
		expect(() => r.updateTag(created.id, { ...NEW_TAG, expectedRevision: 1 })).toThrow(
			DemoRevisionConflictError
		);
		r.deleteTag(created.id);
		expect(r.catalog().tags).toHaveLength(20);
	});

	it('同じグループの名前の重複・存在しないグループは検証で断る', () => {
		const r = new HubRegistry();
		expect(() => r.createTag({ ...NEW_TAG, name: 'pump_flow' })).toThrow(DemoValidationError);
		expect(() => r.createTag({ ...NEW_TAG, collectionGroupId: 99 })).toThrow(DemoValidationError);
	});

	it('一括の作成は 1 本でもエラーなら 1 本も作らない', () => {
		const r = new HubRegistry();
		const result = r.createTagsBatch([NEW_TAG, { ...NEW_TAG, name: '' }], false);
		expect(result.ok).toBe(false);
		expect(r.tags).toHaveLength(20);
		expect(r.createTagsBatch([NEW_TAG], true).ok).toBe(true);
		expect(r.tags).toHaveLength(20);
		expect(r.createTagsBatch([NEW_TAG], false).ok).toBe(true);
		expect(r.tags).toHaveLength(21);
	});
});

/** 模擬の WebSocket の相手（画面）の代わり。 */
function fakeConnection() {
	const sent: Record<string, unknown>[] = [];
	let onMessage: (data: string) => void = () => {};
	let onClose: () => void = () => {};
	const connection: DemoSocketConnection = {
		url: new URL('http://localhost/api/v1/stream'),
		params: {},
		protocols: [],
		send: (data) => sent.push(JSON.parse(data) as Record<string, unknown>),
		close: () => onClose(),
		onMessage: (l) => (onMessage = l),
		onClose: (l) => (onClose = l as () => void)
	};
	return {
		connection,
		sent,
		say: (m: unknown) => onMessage(JSON.stringify(m)),
		hangUp: () => onClose()
	};
}

describe('/api/v1/stream の模擬', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('購読の直後に 1 回のスナップショット、以後は周期で変わった値だけ', () => {
		let now = 1_700_000_000_000;
		const registry = new HubRegistry();
		const peer = fakeConnection();
		createHubSocketRoute(registry, () => now).connect(peer.connection);
		peer.say({ op: 'subscribe', id: 1, tags: ['line1.press.*'], mode: 'on_change' });
		const snapshot = peer.sent[0] as { op: string; id: number; values: { tag: string }[] };
		expect(snapshot.op).toBe('data');
		expect(snapshot.id).toBe(1);
		expect(snapshot.values).toHaveLength(10);
		expect(snapshot.values.every((v) => v.tag.startsWith('line1.press.'))).toBe(true);

		// 数十秒進めると、変わった値（bit の切り替えなど）だけの差分が届く。
		const before = peer.sent.length;
		for (let i = 0; i < 120; i++) {
			now += 250;
			vi.advanceTimersByTime(250);
		}
		const deltas = peer.sent.slice(before) as { values: { id: number }[] }[];
		expect(deltas.length).toBeGreaterThan(0);
		for (const d of deltas) expect(d.values.length).toBeLessThanOrEqual(10);
	});

	it('ids の購読・登録の変更で config_changed・解除でタイマーが止まる', () => {
		const registry = new HubRegistry();
		const peer = fakeConnection();
		createHubSocketRoute(registry).connect(peer.connection);
		peer.say({ op: 'subscribe', id: 7, ids: [1, 2] });
		expect((peer.sent[0] as { values: unknown[] }).values).toHaveLength(2);
		registry.createTag(NEW_TAG);
		expect(peer.sent.some((m) => m.op === 'config_changed')).toBe(true);

		peer.hangUp();
		const count = peer.sent.length;
		vi.advanceTimersByTime(5000);
		registry.createTag({ ...NEW_TAG, name: 'again' });
		expect(peer.sent).toHaveLength(count);
		expect(vi.getTimerCount()).toBe(0);
	});

	it('形の悪い購読は error、ping は pong', () => {
		const peer = fakeConnection();
		createHubSocketRoute(new HubRegistry()).connect(peer.connection);
		peer.say({ op: 'subscribe', id: 1, tags: [] });
		peer.say({ op: 'subscribe', id: 2, tags: ['a.*'] });
		peer.say({ op: 'ping' });
		expect(peer.sent.map((m) => m.op)).toEqual(['error', 'error', 'pong']);
		peer.hangUp();
	});
});
