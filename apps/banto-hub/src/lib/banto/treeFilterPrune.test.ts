import { describe, expect, it } from 'vitest';
import { pruneTreeFilter, type TreeFilterSelection } from './treeFilterPrune';

const CONNECTIONS = [{ id: 1 }, { id: 2 }];
const GROUPS = [{ id: 10 }, { id: 11 }];

describe('pruneTreeFilter', () => {
	it('生きている選択はそのまま（同一参照）返す', () => {
		const connection: TreeFilterSelection = { type: 'connection', id: 2 };
		const group: TreeFilterSelection = { type: 'group', id: 11 };
		expect(pruneTreeFilter(connection, CONNECTIONS, GROUPS)).toBe(connection);
		expect(pruneTreeFilter(group, CONNECTIONS, GROUPS)).toBe(group);
	});

	it('「すべて」はそのまま返す', () => {
		const all: TreeFilterSelection = { type: 'all' };
		expect(pruneTreeFilter(all, CONNECTIONS, GROUPS)).toBe(all);
	});

	it('消えた接続を指していたら「すべて」へ戻す', () => {
		expect(pruneTreeFilter({ type: 'connection', id: 99 }, CONNECTIONS, GROUPS)).toEqual({
			type: 'all'
		});
	});

	it('消えた収集グループを指していたら「すべて」へ戻す', () => {
		expect(pruneTreeFilter({ type: 'group', id: 99 }, CONNECTIONS, GROUPS)).toEqual({
			type: 'all'
		});
	});

	it('接続とグループを取り違えない（同じ id でも別の一覧を見る）', () => {
		// id=1 は接続には有るがグループには無い（逆も同様）。
		expect(pruneTreeFilter({ type: 'group', id: 1 }, CONNECTIONS, GROUPS)).toEqual({ type: 'all' });
		expect(pruneTreeFilter({ type: 'connection', id: 10 }, CONNECTIONS, GROUPS)).toEqual({
			type: 'all'
		});
	});

	it('両方の一覧が空のときは触らない（未ロードと全削除を区別できないため）', () => {
		const filter: TreeFilterSelection = { type: 'group', id: 10 };
		expect(pruneTreeFilter(filter, [], [])).toBe(filter);
	});

	it('片方だけ空のときは判定する（接続だけ残っていてグループが全滅した等）', () => {
		expect(pruneTreeFilter({ type: 'group', id: 10 }, CONNECTIONS, [])).toEqual({ type: 'all' });
		expect(pruneTreeFilter({ type: 'connection', id: 1 }, CONNECTIONS, [])).toEqual({
			type: 'connection',
			id: 1
		});
	});
});
