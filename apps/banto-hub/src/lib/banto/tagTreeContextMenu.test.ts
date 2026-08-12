/**
 * `tagTreeContextMenu.ts`（T18-2e、docs/banto-hub-desktop-plan.md §9.4
 * TAG-UX-A）のユニットテスト。`tagOnboarding.test.ts`/`tagFormCarry.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import type { PlcConnection, CollectionGroup } from './tagRegistryAdmin';
import type { ConnectionTreeNodeData } from '$lib/components/connectionTreeTypes';
import { resolveTagTreeContextMenuAction } from './tagTreeContextMenu';

function connection(overrides: Partial<PlcConnection> = {}): PlcConnection {
	return {
		id: 1,
		name: 'plc1',
		protocol: 'modbus-tcp',
		host: '127.0.0.1',
		port: 502,
		unitId: 1,
		enabled: true,
		simulation: true,
		wordOrder: 'low_high',
		...overrides
	};
}

function group(overrides: Partial<CollectionGroup> = {}): CollectionGroup {
	return {
		id: 1,
		name: 'group1',
		plcConnectionId: 1,
		periodMs: 1000,
		enabled: true,
		...overrides
	};
}

describe('resolveTagTreeContextMenuAction', () => {
	it('"すべて" ノード（ルート）は接続作成へのリンクを返す', () => {
		const data: ConnectionTreeNodeData = { kind: 'all' };
		expect(resolveTagTreeContextMenuAction(data)).toEqual({
			kind: 'createConnection',
			label: 'PLC接続を作成',
			href: '/plc-connections'
		});
	});

	it('実接続ノードは、その接続 ID をプリセットしたグループ作成リンクを返す', () => {
		const conn = connection({ id: 7, name: 'line-a' });
		const data: ConnectionTreeNodeData = { kind: 'connection', connection: conn };
		expect(resolveTagTreeContextMenuAction(data)).toEqual({
			kind: 'createGroup',
			label: 'line-a 配下に収集グループを作成',
			connectionId: 7,
			href: '/collection-groups?connectionId=7'
		});
	});

	it('virtual（calc/mem）接続ノードは null（メニューを出さない）', () => {
		const calc = connection({ id: 2, name: 'calc', protocol: 'virtual' });
		const data: ConnectionTreeNodeData = { kind: 'connection', connection: calc };
		expect(resolveTagTreeContextMenuAction(data)).toBeNull();
	});

	it('実接続配下のグループノードは、そのグループ ID をプリセットしたタグ作成アクションを返す', () => {
		const conn = connection({ id: 3, protocol: 'slmp' });
		const g = group({ id: 9, name: 'temps', plcConnectionId: 3 });
		const data: ConnectionTreeNodeData = { kind: 'group', group: g, connection: conn };
		expect(resolveTagTreeContextMenuAction(data)).toEqual({
			kind: 'createTag',
			label: 'temps 配下にタグを作成',
			groupId: 9
		});
	});

	it('virtual（calc/mem）接続配下のグループノードは null（メニューを出さない）', () => {
		const mem = connection({ id: 4, name: 'mem', protocol: 'virtual' });
		const g = group({ id: 10, name: 'internal-tags', plcConnectionId: 4 });
		const data: ConnectionTreeNodeData = { kind: 'group', group: g, connection: mem };
		expect(resolveTagTreeContextMenuAction(data)).toBeNull();
	});

	it('メニュー項目の文言は既存 e2e が参照する成功トースト/ボタン名と部分一致しない', () => {
		// T18-2d の教訓（PR #135 CI 回帰）: 新規テキストは既存 spec の
		// getByText/getByRole と部分一致しないことを静的に固定する。
		const collisionStrings = [
			'作成しました',
			'更新しました',
			'削除しました',
			'新規登録',
			'新規作成',
			'登録して次へ',
			'登録して閉じる'
		];
		const conn = connection({ id: 1, name: 'plc1' });
		const g = group({ id: 1, plcConnectionId: 1 });
		const actions = [
			resolveTagTreeContextMenuAction({ kind: 'all' }),
			resolveTagTreeContextMenuAction({ kind: 'connection', connection: conn }),
			resolveTagTreeContextMenuAction({ kind: 'group', group: g, connection: conn })
		];
		for (const action of actions) {
			expect(action).not.toBeNull();
			for (const collision of collisionStrings) {
				expect(action!.label.includes(collision)).toBe(false);
				expect(collision.includes(action!.label)).toBe(false);
			}
		}
	});
});
