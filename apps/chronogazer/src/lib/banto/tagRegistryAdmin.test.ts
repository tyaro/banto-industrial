/**
 * `tagRegistryAdmin.ts` の Tauri/REST 分岐とデモモード拒否のユニットテスト
 * （#383 段階2a / R1-B）。`usersAdmin.ts` にはテストが無いため、同じリポジトリ
 * 内の同種クライアント（`apps/banto-hub/src/lib/banto/tagRegistryAdmin.test.ts`
 * の doc comment 参照）が採る回避策に倣う: `tagRegistryAdmin.ts` は
 * `@tauri-apps/api/core`（Tauri IPC、テスト環境には無い）・`@banto/admin-core`
 * （Svelte 5 rune を使う `.svelte.ts` を推移的に import する）・`./setup`
 * （`$lib/toast.svelte` を import する）をトップレベルで import しており、
 * このリポジトリの最小 vitest 構成（`@sveltejs/vite-plugin-svelte` 無し、
 * `$lib` エイリアス無し）ではそのままロードできない。3つとも `vi.mock` で
 * 軽量なフェイクに差し替えることで、実モジュールの副作用を評価せずに
 * `tagRegistryAdmin.ts` 本体（`invoke`/`fetch` 分岐を含む）をロード・
 * テストできる。
 *
 * 固定したい核心:
 * 1. デモモード（`getBantoMode() === 'demo'`）では全呼び出しが
 *    `DEMO_MODE_MESSAGE` で reject される（`invoke`/`fetch` のどちらも
 *    呼ばれない）。
 * 2. Tauri モードでは `invoke()` が正しいコマンド名・引数で呼ばれる。
 * 3. サーバーモードでは `fetch()` が正しい REST パス・メソッド・
 *    ヘッダー（CSRFヘッダー・Bearer トークン）で呼ばれる。
 * 4. REST のエラー応答（`{kind:"validation", field_errors:[...]}`）が
 *    `ProviderError` としてそのまま投げられる（画面がフィールドへ
 *    マッピングできるように）。
 */
import { describe, expect, it, vi, beforeEach } from 'vitest';

// `vi.mock`のファクトリは import 文より前にホイストされるため、ここで参照
// する可変状態・クラスは `vi.hoisted` で用意する（素の `let` を外側に
// 置くと、ファクトリ評価時点でまだ初期化されていない - TDZ で落ちる）。
const testState = vi.hoisted(() => {
	// `describe()`相当のミニ版（`@banto/admin-core`の`errors.ts`と同じ
	// マッピング）。テストが使う kind（validation/other）だけ本物と同じ
	// 文言にする - `.message` を見るテスト（デモモード等）と
	// `.body.field_errors` を見るテスト（validation）の両方が本物と
	// 同じ挙動になる。
	function describe(body: { kind: string; message?: string }): string {
		if (body.kind === 'validation') return 'validation failed';
		return typeof body.message === 'string' ? body.message : 'provider error';
	}
	class ProviderErrorFake extends Error {
		body: unknown;
		constructor(body: { kind: string; message?: string }) {
			super(describe(body));
			this.name = 'ProviderError';
			this.body = body;
		}
	}
	return {
		bantoMode: 'demo' as 'tauri' | 'server' | 'demo',
		ProviderErrorFake
	};
});

vi.mock('@tauri-apps/api/core', () => ({
	invoke: vi.fn()
}));

vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => 'test-token' }),
	isProviderError: (err: unknown) => err instanceof testState.ProviderErrorFake,
	ProviderError: testState.ProviderErrorFake
}));

vi.mock('./setup', () => ({
	CSRF_HEADER: { 'X-Banto-Client': 'banto' },
	getBantoMode: () => testState.bantoMode
}));

import { invoke } from '@tauri-apps/api/core';

import {
	listPlcConnections,
	createPlcConnection,
	updatePlcConnection,
	deletePlcConnection,
	listCollectionGroups,
	createCollectionGroup,
	listTags,
	createTag,
	deleteTag,
	isTagRegistryAvailable,
	DEMO_MODE_MESSAGE,
	ALLOWED_PERIOD_MS,
	type PlcConnectionInput,
	type CollectionGroupInput,
	type TagInput
} from './tagRegistryAdmin';

const connectionInput: PlcConnectionInput = {
	name: 'plc1',
	protocol: 'modbus-tcp',
	host: '192.168.11.200',
	port: 502,
	unitId: 1,
	enabled: true,
	wordOrder: ''
};

const groupInput: CollectionGroupInput = {
	name: 'group1',
	plcConnectionId: 1,
	periodMs: 1000,
	enabled: true
};

const tagInput: TagInput = {
	name: 'tag1',
	collectionGroupId: 1,
	address: 'D3000',
	dataType: 'i16',
	decimals: 0,
	enabled: true
};

function mockFetchOnce(response: { status: number; ok: boolean; body: unknown }): void {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => ({
			ok: response.ok,
			status: response.status,
			statusText: 'status',
			json: async () => response.body
		}))
	);
}

beforeEach(() => {
	vi.restoreAllMocks();
	testState.bantoMode = 'demo';
});

describe('isTagRegistryAvailable', () => {
	it('デモモードでは false、それ以外では true', () => {
		testState.bantoMode = 'demo';
		expect(isTagRegistryAvailable()).toBe(false);
		testState.bantoMode = 'tauri';
		expect(isTagRegistryAvailable()).toBe(true);
		testState.bantoMode = 'server';
		expect(isTagRegistryAvailable()).toBe(true);
	});
});

describe('デモモード（バックエンドが無い）', () => {
	it('全エンティティの全操作が DEMO_MODE_MESSAGE で reject され、invoke/fetch は一切呼ばれない', async () => {
		testState.bantoMode = 'demo';
		const fetchSpy = vi.fn();
		vi.stubGlobal('fetch', fetchSpy);

		await expect(listPlcConnections()).rejects.toThrow(DEMO_MODE_MESSAGE);
		await expect(createPlcConnection(connectionInput)).rejects.toThrow(DEMO_MODE_MESSAGE);
		await expect(listCollectionGroups()).rejects.toThrow(DEMO_MODE_MESSAGE);
		await expect(createCollectionGroup(groupInput)).rejects.toThrow(DEMO_MODE_MESSAGE);
		await expect(listTags()).rejects.toThrow(DEMO_MODE_MESSAGE);
		await expect(createTag(tagInput)).rejects.toThrow(DEMO_MODE_MESSAGE);

		expect(invoke).not.toHaveBeenCalled();
		expect(fetchSpy).not.toHaveBeenCalled();
	});
});

describe('Tauri モード', () => {
	it('listPlcConnections は invoke("plc_connections_list") を引数なしで呼ぶ', async () => {
		testState.bantoMode = 'tauri';
		vi.mocked(invoke).mockResolvedValueOnce([]);
		await listPlcConnections();
		expect(invoke).toHaveBeenCalledWith('plc_connections_list', undefined);
	});

	it('createPlcConnection は invoke("plc_connections_create", {input}) を呼ぶ', async () => {
		testState.bantoMode = 'tauri';
		vi.mocked(invoke).mockResolvedValueOnce({ id: 1, ...connectionInput });
		await createPlcConnection(connectionInput);
		expect(invoke).toHaveBeenCalledWith('plc_connections_create', { input: connectionInput });
	});

	it('updatePlcConnection は invoke("plc_connections_update", {id, input}) を呼ぶ', async () => {
		testState.bantoMode = 'tauri';
		vi.mocked(invoke).mockResolvedValueOnce({ id: 5, ...connectionInput });
		await updatePlcConnection(5, connectionInput);
		expect(invoke).toHaveBeenCalledWith('plc_connections_update', {
			id: 5,
			input: connectionInput
		});
	});

	it('deletePlcConnection は invoke("plc_connections_delete", {id}) を呼ぶ', async () => {
		testState.bantoMode = 'tauri';
		vi.mocked(invoke).mockResolvedValueOnce(undefined);
		await deletePlcConnection(7);
		expect(invoke).toHaveBeenCalledWith('plc_connections_delete', { id: 7 });
	});

	it('収集グループ・タグも同じ作法で invoke する', async () => {
		testState.bantoMode = 'tauri';
		vi.mocked(invoke).mockResolvedValueOnce({ id: 1, ...groupInput });
		await createCollectionGroup(groupInput);
		expect(invoke).toHaveBeenCalledWith('collection_groups_create', { input: groupInput });

		vi.mocked(invoke).mockResolvedValueOnce({ id: 1, ...tagInput });
		await createTag(tagInput);
		expect(invoke).toHaveBeenCalledWith('tags_create', { input: tagInput });

		vi.mocked(invoke).mockResolvedValueOnce(undefined);
		await deleteTag(3);
		expect(invoke).toHaveBeenCalledWith('tags_delete', { id: 3 });
	});

	it('invoke が例外を投げたら field_errors を保った ProviderError として reject する', async () => {
		testState.bantoMode = 'tauri';
		vi.mocked(invoke).mockRejectedValueOnce({
			kind: 'validation',
			field_errors: [{ field: 'protocol', message: '対応していません' }]
		});
		await expect(createPlcConnection(connectionInput)).rejects.toMatchObject({
			body: {
				kind: 'validation',
				field_errors: [{ field: 'protocol', message: '対応していません' }]
			}
		});
	});
});

describe('サーバーモード（REST）', () => {
	it('listPlcConnections は GET /api/plc-connections を CSRF/Authorization ヘッダー付きで叩く', async () => {
		testState.bantoMode = 'server';
		mockFetchOnce({ status: 200, ok: true, body: [] });
		await listPlcConnections();
		expect(fetch).toHaveBeenCalledWith(
			'/api/plc-connections',
			expect.objectContaining({
				method: 'GET',
				headers: expect.objectContaining({
					'X-Banto-Client': 'banto',
					Authorization: 'Bearer test-token'
				})
			})
		);
	});

	it('createPlcConnection は POST + JSON body で叩く', async () => {
		testState.bantoMode = 'server';
		mockFetchOnce({ status: 200, ok: true, body: { id: 1, ...connectionInput } });
		await createPlcConnection(connectionInput);
		expect(fetch).toHaveBeenCalledWith(
			'/api/plc-connections',
			expect.objectContaining({
				method: 'POST',
				body: JSON.stringify(connectionInput),
				headers: expect.objectContaining({ 'Content-Type': 'application/json' })
			})
		);
	});

	it('updatePlcConnection/deletePlcConnection は id 付きパスを叩く', async () => {
		testState.bantoMode = 'server';
		mockFetchOnce({ status: 200, ok: true, body: { id: 9, ...connectionInput } });
		await updatePlcConnection(9, connectionInput);
		expect(fetch).toHaveBeenCalledWith(
			'/api/plc-connections/9',
			expect.objectContaining({ method: 'PUT' })
		);

		mockFetchOnce({ status: 204, ok: true, body: undefined });
		await deletePlcConnection(9);
		expect(fetch).toHaveBeenCalledWith(
			'/api/plc-connections/9',
			expect.objectContaining({ method: 'DELETE' })
		);
	});

	it('収集グループ・タグも対応する REST パスを叩く', async () => {
		testState.bantoMode = 'server';
		mockFetchOnce({ status: 200, ok: true, body: [] });
		await listCollectionGroups();
		expect(fetch).toHaveBeenCalledWith(
			'/api/collection-groups',
			expect.objectContaining({ method: 'GET' })
		);

		mockFetchOnce({ status: 200, ok: true, body: { id: 1, ...tagInput } });
		await createTag(tagInput);
		expect(fetch).toHaveBeenCalledWith('/api/tags', expect.objectContaining({ method: 'POST' }));
	});

	// R1-B の完了条件: 検証エラーが人間可読な `field_errors` のまま
	// `ProviderError` として届く（画面が `setServerErrors` でフィールドへ
	// マッピングできる形）。
	it('422 validation 応答は field_errors を保った ProviderError になる', async () => {
		testState.bantoMode = 'server';
		mockFetchOnce({
			status: 422,
			ok: false,
			body: {
				kind: 'validation',
				field_errors: [{ field: 'periodMs', message: '周期は 100, 200, ... のいずれかです' }]
			}
		});
		await expect(createCollectionGroup(groupInput)).rejects.toMatchObject({
			body: {
				kind: 'validation',
				field_errors: [{ field: 'periodMs', message: expect.stringContaining('周期') }]
			}
		});
	});

	it('ネットワークエラーは専用メッセージの ProviderError になる', async () => {
		testState.bantoMode = 'server';
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new Error('network down');
			})
		);
		await expect(listTags()).rejects.toThrow('サーバーに接続できません');
	});
});

describe('ALLOWED_PERIOD_MS', () => {
	it('banto_tags::ALLOWED_PERIOD_MS と同じ8値', () => {
		expect(ALLOWED_PERIOD_MS).toEqual([100, 200, 500, 1000, 2000, 5000, 10000, 60000]);
	});
});
