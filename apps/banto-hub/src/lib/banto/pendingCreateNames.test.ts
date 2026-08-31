/**
 * `pendingCreateNames.ts` に対するユニットテスト。実機で再現した不具合
 * （2026-08-31 オーナー報告 - 修正1）の根本原因（pending queue 内の
 * 未適用の作成分を連番プリフィルが見ていなかった）を直接検証する。
 */
import { describe, expect, it } from 'vitest';
import { pendingCreateNames, type PendingChangeLike } from './pendingCreateNames';

function pendingCreate(source: string, name: string, state = 'pending'): PendingChangeLike {
	return { state, source, payload: { input: { name } } };
}

describe('pendingCreateNames', () => {
	it('pending が空なら空配列を返す', () => {
		expect(pendingCreateNames([], 'collection_groups.create')).toEqual([]);
	});

	it('state=pending かつ source が一致する作成分の名前だけを返す', () => {
		const pending: PendingChangeLike[] = [
			pendingCreate('collection_groups.create', 'group1'),
			pendingCreate('collection_groups.create', 'group2')
		];
		expect(pendingCreateNames(pending, 'collection_groups.create')).toEqual(['group1', 'group2']);
	});

	it('実機再現ケース: 収集稼働中に同じ名前で3回作成した pending 3件が全て候補になる', () => {
		// オーナーが実機で再現した状況そのもの: pending #2/#3/#4 が全部
		// name="group1" で積まれている（適用はまだされていない）。
		const pending: PendingChangeLike[] = [
			pendingCreate('collection_groups.create', 'group1'),
			pendingCreate('collection_groups.create', 'group1'),
			pendingCreate('collection_groups.create', 'group1')
		];
		expect(pendingCreateNames(pending, 'collection_groups.create')).toEqual([
			'group1',
			'group1',
			'group1'
		]);
	});

	it('pending 以外の state（applying/applied/canceled/failed）は除外する', () => {
		const pending: PendingChangeLike[] = [
			pendingCreate('collection_groups.create', 'applying-name', 'applying'),
			pendingCreate('collection_groups.create', 'applied-name', 'applied'),
			pendingCreate('collection_groups.create', 'canceled-name', 'canceled'),
			pendingCreate('collection_groups.create', 'failed-name', 'failed'),
			pendingCreate('collection_groups.create', 'group1', 'pending')
		];
		expect(pendingCreateNames(pending, 'collection_groups.create')).toEqual(['group1']);
	});

	it('source が一致しない pending（update/delete や別リソース）は除外する', () => {
		const pending: PendingChangeLike[] = [
			{
				state: 'pending',
				source: 'collection_groups.update',
				payload: { id: 1, input: { name: 'renamed' } }
			},
			{ state: 'pending', source: 'collection_groups.delete', payload: { id: 1 } },
			{
				state: 'pending',
				source: 'plc_connections.create',
				payload: { input: { name: 'connection1' } }
			},
			pendingCreate('collection_groups.create', 'group1')
		];
		expect(pendingCreateNames(pending, 'collection_groups.create')).toEqual(['group1']);
	});

	it('plc_connections.create の source を指定すれば PLC接続側の名前を抽出する', () => {
		const pending: PendingChangeLike[] = [
			pendingCreate('plc_connections.create', 'connection1'),
			pendingCreate('collection_groups.create', 'group1')
		];
		expect(pendingCreateNames(pending, 'plc_connections.create')).toEqual(['connection1']);
	});

	it('payload の形が想定と違う（input が無い/name が無い/name が文字列でない）場合は読み飛ばす', () => {
		const pending: PendingChangeLike[] = [
			{ state: 'pending', source: 'collection_groups.create', payload: {} },
			{ state: 'pending', source: 'collection_groups.create', payload: { input: {} } },
			{ state: 'pending', source: 'collection_groups.create', payload: { input: { name: 123 } } },
			{ state: 'pending', source: 'collection_groups.create', payload: null },
			{ state: 'pending', source: 'collection_groups.create', payload: 'not-an-object' },
			pendingCreate('collection_groups.create', 'group1')
		];
		expect(pendingCreateNames(pending, 'collection_groups.create')).toEqual(['group1']);
	});
});
