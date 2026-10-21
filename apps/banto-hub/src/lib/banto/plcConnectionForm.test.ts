/**
 * `plcConnectionForm.ts`（T18-6a）に対するユニットテスト。`tagFormLayout.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	DEFAULT_PORTS,
	DEFAULT_WORD_ORDERS,
	PROTOCOL_OPTIONS,
	blankConnectionForm,
	connectionToForm,
	defaultPortFor,
	defaultWordOrderFor,
	formToConnectionInput,
	isDefaultPortForProtocol,
	initialWordOrderTouched,
	isDefaultWordOrderForProtocol,
	nextConnectionName,
	passwordForSubmit,
	validatePostgresFields,
	blankBridgeQuery,
	formatBridgeReferenceDeleteRefusal,
	ingestEndpoints,
	isSavedPushConnection,
	resolveSavedConnection,
	mapPushFieldErrors,
	parseBridgeColumns,
	pushBridgeLoad,
	pushBridgesReferencing,
	pushFormFromSettings,
	pushSettingsFromForm,
	validatePushFields,
	type PlcConnectionFormState
} from './plcConnectionForm';
import { isFormDirty } from './formDirty';
import type { PlcConnection } from './tagRegistryAdmin';

describe('nextConnectionName', () => {
	it('既存名が無ければ connection1 を返す', () => {
		expect(nextConnectionName([])).toBe('connection1');
	});

	it('実装指示の例: connection1, connection3 があれば connection2 を返す（歯抜けを埋める）', () => {
		expect(nextConnectionName(['connection1', 'connection3'])).toBe('connection2');
	});

	it('連続した既存名の直後（歯抜けなし）は最大値+1を返す', () => {
		expect(nextConnectionName(['connection1', 'connection2', 'connection3'])).toBe('connection4');
	});

	it('無関係な名前（calc/mem や自由入力）は無視する', () => {
		expect(nextConnectionName(['calc', 'mem', 'ライン1', 'connection1'])).toBe('connection2');
	});

	it('接尾辞付きの名前（connection1-old 等）は番号として扱わない', () => {
		expect(nextConnectionName(['connection1-old', 'connection1'])).toBe('connection2');
	});

	it('prefix を明示指定できる', () => {
		expect(nextConnectionName(['line1'], 'line')).toBe('line2');
	});
});

describe('nextConnectionName（修正1: pendingNames — 実機で再現した不具合、2026-08-31 オーナー報告）', () => {
	it('既存 connection1 に加え pending の connection1 があっても connection2 を返す（収集グループ側と同じ不具合が接続側にもあった）', () => {
		expect(nextConnectionName(['connection1'], 'connection', ['connection1'])).toBe('connection2');
	});

	it('pendingNames が空なら既存レコードのみの場合と同じ結果になる（回帰確認）', () => {
		expect(nextConnectionName(['connection1', 'connection3'], 'connection', [])).toBe(
			'connection2'
		);
	});

	it('pendingNames を省略しても既存の呼び出し（引数2つ）と同じ結果になる', () => {
		expect(nextConnectionName(['connection1'], 'connection')).toBe('connection2');
	});

	it('pendingNames にしか無い番号も歯抜け埋めの対象として除外する', () => {
		expect(nextConnectionName(['connection1'], 'connection', ['connection2'])).toBe('connection3');
	});
});

describe('defaultPortFor / isDefaultPortForProtocol', () => {
	it('modbus-tcp の既定ポートは 502', () => {
		expect(defaultPortFor('modbus-tcp')).toBe(502);
		expect(DEFAULT_PORTS['modbus-tcp']).toBe(502);
	});

	it('slmp の既定ポートは 5007（crates/banto-plc の SlmpConfig::default() と一致）', () => {
		expect(defaultPortFor('slmp')).toBe(5007);
		expect(DEFAULT_PORTS.slmp).toBe(5007);
	});

	it('virtual は既定ポートを持たない', () => {
		expect(defaultPortFor('virtual')).toBeUndefined();
	});

	it('S1: postgres の既定ポートは 5432（PostgreSQL の標準ポート）', () => {
		expect(defaultPortFor('postgres')).toBe(5432);
		expect(DEFAULT_PORTS.postgres).toBe(5432);
	});

	it('isDefaultPortForProtocol: 既定値と一致すれば true', () => {
		expect(isDefaultPortForProtocol('502', 'modbus-tcp')).toBe(true);
		expect(isDefaultPortForProtocol('5007', 'slmp')).toBe(true);
	});

	it('isDefaultPortForProtocol: 既定値と異なれば false', () => {
		expect(isDefaultPortForProtocol('1502', 'modbus-tcp')).toBe(false);
	});

	it('isDefaultPortForProtocol: 既定を持たないプロトコルは常に false', () => {
		expect(isDefaultPortForProtocol('0', 'virtual')).toBe(false);
	});
});

describe('initialWordOrderTouched（Copilot レビュー指摘: プロトコル切替時の追従漏れ）', () => {
	it('既定と一致していれば「未編集」= false（切替時に追従する）', () => {
		expect(initialWordOrderTouched('high_low', 'modbus-tcp')).toBe(false);
		expect(initialWordOrderTouched('low_high', 'slmp')).toBe(false);
	});

	it('既定と異なれば「ユーザーが選んだ」= true（切替時に上書きしない）', () => {
		expect(initialWordOrderTouched('low_high', 'modbus-tcp')).toBe(true);
		expect(initialWordOrderTouched('high_low', 'slmp')).toBe(true);
	});

	// 回帰防止: ワード順の欄が出ないプロトコル（virtual/postgres）で true に
	// なると、そこから modbus-tcp へ切り替えたときに既定 high_low への追従が
	// 効かず、postgres 由来の low_high がそのまま Modbus 接続として保存される。
	it('既定を持たないプロトコル（virtual/postgres）は保存値によらず false', () => {
		for (const wordOrder of ['low_high', 'high_low'] as const) {
			expect(initialWordOrderTouched(wordOrder, 'virtual')).toBe(false);
			expect(initialWordOrderTouched(wordOrder, 'postgres')).toBe(false);
		}
	});
});

describe('defaultWordOrderFor / isDefaultWordOrderForProtocol（2026-09-08 オーナー決定、issue #325 で発見した既存バグの修正）', () => {
	it('modbus-tcp の既定ワード順は high_low（Modbus/IEEE慣習に統一）', () => {
		expect(defaultWordOrderFor('modbus-tcp')).toBe('high_low');
		expect(DEFAULT_WORD_ORDERS['modbus-tcp']).toBe('high_low');
	});

	it('slmp の既定ワード順は low_high（MELSEC標準、従来どおり）', () => {
		expect(defaultWordOrderFor('slmp')).toBe('low_high');
		expect(DEFAULT_WORD_ORDERS.slmp).toBe('low_high');
	});

	it('virtual/postgres は既定ワード順を持たない', () => {
		expect(defaultWordOrderFor('virtual')).toBeUndefined();
		expect(defaultWordOrderFor('postgres')).toBeUndefined();
	});

	it('isDefaultWordOrderForProtocol: 既定値と一致すれば true', () => {
		expect(isDefaultWordOrderForProtocol('high_low', 'modbus-tcp')).toBe(true);
		expect(isDefaultWordOrderForProtocol('low_high', 'slmp')).toBe(true);
	});

	it('isDefaultWordOrderForProtocol: 既定値と異なれば false', () => {
		expect(isDefaultWordOrderForProtocol('low_high', 'modbus-tcp')).toBe(false);
		expect(isDefaultWordOrderForProtocol('high_low', 'slmp')).toBe(false);
	});

	it('isDefaultWordOrderForProtocol: 既定を持たないプロトコルは常に false', () => {
		expect(isDefaultWordOrderForProtocol('low_high', 'virtual')).toBe(false);
	});
});

describe('blankConnectionForm / connectionToForm / formToConnectionInput', () => {
	it('blankConnectionForm は既定プロトコル（modbus-tcp）の既定ワード順 high_low で初期化する（2026-09-08 オーナー決定、issue #325 の修正）', () => {
		expect(blankConnectionForm()).toEqual({
			name: '',
			protocol: 'modbus-tcp',
			host: '',
			port: '502',
			unitId: '1',
			enabled: true,
			simulation: false,
			wordOrder: 'high_low',
			cpuType: 'R',
			database: '',
			username: '',
			password: '',
			clearPassword: false,
			staleTimeoutMs: '',
			bridgeEnabled: false,
			bridgeDbConnectionId: '',
			bridgeQueries: []
		});
	});

	it('connectionToForm は保存済み接続を文字列化したフォーム状態へ変換する', () => {
		const conn: PlcConnection = {
			id: 7,
			name: 'Line1',
			protocol: 'slmp',
			host: '192.168.1.10',
			port: 5007,
			unitId: 3,
			enabled: true,
			simulation: false,
			wordOrder: 'high_low',
			cpuType: 'R',
			database: null,
			username: null,
			passwordSet: false
		};
		expect(connectionToForm(conn)).toEqual({
			name: 'Line1',
			protocol: 'slmp',
			host: '192.168.1.10',
			port: '5007',
			unitId: '3',
			enabled: true,
			simulation: false,
			wordOrder: 'high_low',
			cpuType: 'R',
			database: '',
			username: '',
			password: '',
			clearPassword: false,
			staleTimeoutMs: '',
			bridgeEnabled: false,
			bridgeDbConnectionId: '',
			bridgeQueries: []
		});
	});

	it('formToConnectionInput は数値フィールドを number へ戻す（往復変換）', () => {
		const conn: PlcConnection = {
			id: 1,
			name: 'X',
			protocol: 'modbus-tcp',
			host: 'h',
			port: 502,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			cpuType: 'R',
			database: null,
			username: null,
			passwordSet: false
		};
		expect(formToConnectionInput(connectionToForm(conn))).toEqual({
			name: 'X',
			protocol: 'modbus-tcp',
			host: 'h',
			port: 502,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			cpuType: 'R'
		});
	});

	it('S1: connectionToForm は保存済みパスワードを絶対にプリフィルしない（passwordSet: true でも password は常に空文字列）', () => {
		const conn: PlcConnection = {
			id: 9,
			name: 'pg1',
			protocol: 'postgres',
			host: '127.0.0.1',
			port: 5432,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			cpuType: 'R',
			database: 'appdb',
			username: 'appuser',
			passwordSet: true
		};
		const form = connectionToForm(conn);
		expect(form.password).toBe('');
		expect(form.clearPassword).toBe(false);
		expect(form.database).toBe('appdb');
		expect(form.username).toBe('appuser');
	});

	it('S1: formToConnectionInput は postgres のときだけ database/username/password を含む', () => {
		const form: PlcConnectionFormState = {
			...blankConnectionForm(),
			protocol: 'postgres',
			host: '127.0.0.1',
			port: '5432',
			database: 'appdb',
			username: 'appuser',
			password: 'hunter2'
		};
		expect(formToConnectionInput(form)).toEqual({
			name: '',
			protocol: 'postgres',
			host: '127.0.0.1',
			port: 5432,
			unitId: 1,
			enabled: true,
			simulation: false,
			// blankConnectionForm() の既定プロトコル modbus-tcp のワード順
			// （high_low）を protocol 切り替え後もそのまま引き継ぐ - この
			// フォーム自体は onProtocolChange を経由しない素の状態遷移
			// （spread による直接上書き）なので、追従ロジックの対象外。
			wordOrder: 'high_low',
			cpuType: 'R',
			database: 'appdb',
			username: 'appuser',
			password: 'hunter2'
		});
	});

	it('S1: formToConnectionInput は非 postgres では database/username/password を一切送らない', () => {
		const form: PlcConnectionFormState = {
			...blankConnectionForm(),
			protocol: 'modbus-tcp',
			host: '127.0.0.1',
			// これらが仮に埋まっていても（フォームを postgres→modbus と
			// 切り替えた直後の残留値等）非postgresでは送らない。
			database: 'leftover-db',
			username: 'leftover-user',
			password: 'leftover-pw'
		};
		const input = formToConnectionInput(form);
		expect(input).not.toHaveProperty('database');
		expect(input).not.toHaveProperty('username');
		expect(input).not.toHaveProperty('password');
	});
});

describe('S1: passwordForSubmit（パスワード tri-state の組み立て）', () => {
	it('clearPassword が true なら password の中身に関わらず "" を返す（消去）', () => {
		expect(passwordForSubmit({ password: 'ignored', clearPassword: true })).toBe('');
		expect(passwordForSubmit({ password: '', clearPassword: true })).toBe('');
	});

	it('password が空文字列（未入力）なら undefined を返す（create: パスワード無し / update: 現在のパスワードを維持）', () => {
		expect(passwordForSubmit({ password: '', clearPassword: false })).toBeUndefined();
	});

	it('password に非空文字列があればそのまま返す（新規設定/置き換え）', () => {
		expect(passwordForSubmit({ password: 'hunter2', clearPassword: false })).toBe('hunter2');
	});
});

describe('S1: validatePostgresFields（サーバー側必須ルールのクライアント側ミラー）', () => {
	it('非 postgres では常にエラー無し', () => {
		expect(validatePostgresFields({ protocol: 'modbus-tcp', database: '', username: '' })).toEqual(
			{}
		);
	});

	it('postgres で database/username が空なら必須エラー（サーバーの required_message と同文言）', () => {
		expect(validatePostgresFields({ protocol: 'postgres', database: '', username: '' })).toEqual({
			database: '必須項目です',
			username: '必須項目です'
		});
	});

	it('postgres で database/username が空白のみでも必須エラー（trim 後で判定）', () => {
		expect(
			validatePostgresFields({ protocol: 'postgres', database: '  ', username: '  ' })
		).toEqual({
			database: '必須項目です',
			username: '必須項目です'
		});
	});

	it('postgres で database/username が MAX_..._LEN（128）を超えると文字数エラー', () => {
		const tooLong = 'a'.repeat(129);
		expect(
			validatePostgresFields({ protocol: 'postgres', database: tooLong, username: tooLong })
		).toEqual({
			database: '128文字以内で入力してください',
			username: '128文字以内で入力してください'
		});
	});

	it('postgres で database/username が両方とも妥当ならエラー無し', () => {
		expect(
			validatePostgresFields({ protocol: 'postgres', database: 'appdb', username: 'appuser' })
		).toEqual({});
	});
});

describe('S1: PROTOCOL_OPTIONS に postgres（DB Source）が含まれる', () => {
	it('postgres オプションを日本語ラベル付きで持つ', () => {
		const postgresOption = PROTOCOL_OPTIONS.find((opt) => opt.value === 'postgres');
		expect(postgresOption).toBeDefined();
		expect(postgresOption?.label).toBe('PostgreSQL（DB Source）');
	});
});

/**
 * 2026-09-15 追補（誤爆防止、`ConnectionDrawer.svelte::baseline`/`dirty`）:
 * `ConnectionDrawer.svelte` 自体は Svelte コンポーネントでユニットテスト
 * 対象外（`formDirty.test.ts` 冒頭コメント参照）のため、実際に使う
 * `blankConnectionForm`/`connectionToForm`（baseline の作り方そのもの）と
 * `isFormDirty` を組み合わせて、Drawer が行う判定と同じ形をここで固定する。
 * 実 DOM での確認は
 * `e2e/tests-banto-hub/banto-hub-tags-drawer-accidental-close.spec.ts` に譲る。
 */
describe('ConnectionDrawer の dirty 判定（baseline は開いた時点のフォームスナップショット）', () => {
	it('新規作成: 開いた直後（baseline = blankConnectionForm 相当）は dirty ではない', () => {
		const baseline = blankConnectionForm();
		const form = { ...baseline };
		expect(isFormDirty(baseline, form)).toBe(false);
	});

	it('新規作成: 名前を1文字変えると dirty になる', () => {
		const baseline = blankConnectionForm();
		const form = { ...baseline, name: 'a' };
		expect(isFormDirty(baseline, form)).toBe(true);
	});

	it('再設定: 開いた直後（baseline = connectionToForm(connection)）は dirty ではない', () => {
		const conn: PlcConnection = {
			id: 7,
			name: 'Line1',
			protocol: 'slmp',
			host: '192.168.1.10',
			port: 5007,
			unitId: 3,
			enabled: true,
			simulation: false,
			wordOrder: 'high_low',
			cpuType: 'R',
			database: null,
			username: null,
			passwordSet: false
		};
		const baseline = connectionToForm(conn);
		const form = { ...baseline };
		expect(isFormDirty(baseline, form)).toBe(false);
	});

	it('再設定: ホストを1文字変えると dirty になる', () => {
		const conn: PlcConnection = {
			id: 7,
			name: 'Line1',
			protocol: 'slmp',
			host: '192.168.1.10',
			port: 5007,
			unitId: 3,
			enabled: true,
			simulation: false,
			wordOrder: 'high_low',
			cpuType: 'R',
			database: null,
			username: null,
			passwordSet: false
		};
		const baseline = connectionToForm(conn);
		const form = { ...baseline, host: '192.168.1.11' };
		expect(isFormDirty(baseline, form)).toBe(true);
	});

	it('readOnly: 入力欄が disabled で form が変わらない前提なら、baseline と同一のまま dirty にならない（readOnly 専用の分岐は無く isFormDirty だけで足りる）', () => {
		const conn: PlcConnection = {
			id: 9,
			name: 'ViewOnly',
			protocol: 'modbus-tcp',
			host: '10.0.0.1',
			port: 502,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'high_low',
			cpuType: 'R',
			database: null,
			username: null,
			passwordSet: false
		};
		const baseline = connectionToForm(conn);
		// readOnly では <input disabled> によりユーザーは form を変更できない
		// - つまり form は常に baseline と同一のまま。
		const form = baseline;
		expect(isFormDirty(baseline, form)).toBe(false);
	});
});

// --- R2a-2e: 取り込み口 push（docs/banto-hub-ingest-design.md §3.2・§7.1） ---

function pushForm(overrides: Partial<PlcConnectionFormState> = {}): PlcConnectionFormState {
	return {
		...blankConnectionForm(),
		name: 'Bridge1',
		protocol: 'push',
		...overrides
	};
}

const PUSH_CONNECTION: PlcConnection = {
	id: 12,
	name: 'Bridge1',
	protocol: 'push',
	host: '',
	port: 0,
	unitId: 1,
	enabled: true,
	simulation: false,
	wordOrder: 'low_high',
	cpuType: 'R',
	database: null,
	username: null,
	passwordSet: false,
	settings: {
		stale_timeout_ms: 5000,
		bridge: {
			kind: 'db',
			db_connection_id: 3,
			queries: [
				{
					name: 'line1',
					period_ms: 1000,
					sql: 'SELECT temp, run FROM line1_now',
					columns: [
						{ column: 'temp', key: 'line1.temp' },
						{ column: 'run', key: 'line1.run' }
					]
				}
			]
		}
	}
};

describe('R2a-2e: PROTOCOL_OPTIONS に push（取り込み）が含まれる', () => {
	it('push を日本語ラベル付きで持つ', () => {
		expect(PROTOCOL_OPTIONS.find((opt) => opt.value === 'push')?.label).toBe(
			'取り込み（push・送り手が値を送る）'
		);
	});
});

describe('R2a-2e: push の settings とフォームの往復', () => {
	it('connectionToForm → formToConnectionInput で settings がそのまま戻る', () => {
		const form = connectionToForm(PUSH_CONNECTION);
		expect(form.staleTimeoutMs).toBe('5000');
		expect(form.bridgeEnabled).toBe(true);
		expect(form.bridgeDbConnectionId).toBe('3');
		expect(form.bridgeQueries).toEqual([
			{
				name: 'line1',
				periodMs: 1000,
				sql: 'SELECT temp, run FROM line1_now',
				columnsText: 'temp=line1.temp\nrun=line1.run'
			}
		]);
		const input = formToConnectionInput(form);
		expect(input.settings).toEqual(PUSH_CONNECTION.settings);
		// push は接続先を持たない（サーバーが正規化する）。postgres の項目も送らない。
		expect(input.host).toBe('');
		expect(input.port).toBe(0);
		expect(input.simulation).toBe(false);
		expect(input).not.toHaveProperty('database');
		expect(input).not.toHaveProperty('password');
	});

	it('既定のまま（settings: null）の push は空の settings を送る（update で保存済みの値を消せるように）', () => {
		const form = connectionToForm({ ...PUSH_CONNECTION, settings: null });
		expect(form.staleTimeoutMs).toBe('');
		expect(form.bridgeEnabled).toBe(false);
		expect(formToConnectionInput(form).settings).toEqual({});
	});

	it('ブリッジを外すと settings から bridge が消える', () => {
		const form = { ...connectionToForm(PUSH_CONNECTION), bridgeEnabled: false };
		expect(pushSettingsFromForm(form)).toEqual({ stale_timeout_ms: 5000 });
	});

	it('push 以外の接続は settings を送らない', () => {
		expect(formToConnectionInput(blankConnectionForm())).not.toHaveProperty('settings');
		expect(pushFormFromSettings(undefined).bridgeQueries).toEqual([]);
	});

	it('blankBridgeQuery は既存のクエリ名と衝突しない query{n} を付ける', () => {
		const first = blankBridgeQuery([]);
		expect(first).toEqual({ name: 'query1', periodMs: 1000, sql: '', columnsText: '' });
		expect(blankBridgeQuery([first]).name).toBe('query2');
	});
});

describe('R2a-2e: parseBridgeColumns（1 行に 1 つの「列名=キー」）', () => {
	it('空行を飛ばし、前後の空白を除く', () => {
		expect(parseBridgeColumns(' temp = line1.temp \n\nrun=line1/温度 1\r\n')).toEqual({
			columns: [
				{ column: 'temp', key: 'line1.temp' },
				{ column: 'run', key: 'line1/温度 1' }
			],
			error: null
		});
	});

	it('形の誤りは行番号つきで返す', () => {
		expect(parseBridgeColumns('temp').error).toBe('1 行目: 「列名=キー」の形で書いてください');
		expect(parseBridgeColumns('ok=k\n1bad=k').error).toContain('2 行目: 列名は結果列名');
		expect(parseBridgeColumns('temp=').error).toBe('1 行目: キーを書いてください');
		expect(parseBridgeColumns(`temp=${'k'.repeat(257)}`).error).toBe(
			'1 行目: キーは 256 文字までです'
		);
		expect(parseBridgeColumns(`temp=${'あ'.repeat(256)}`).error).toBeNull();
		expect(parseBridgeColumns('temp=a\u0007b').error).toBe('1 行目: キーに制御文字は使えません');
	});
});

describe('R2a-2e: validatePushFields（保存前の検証）', () => {
	it('push 以外・ブリッジなしで既定のままなら何も出さない', () => {
		expect(validatePushFields(blankConnectionForm())).toEqual({});
		expect(validatePushFields(pushForm())).toEqual({});
	});

	it('受信のタイムアウトは 1,000〜3,600,000 の整数', () => {
		expect(validatePushFields(pushForm({ staleTimeoutMs: '1000' }))).toEqual({});
		expect(validatePushFields(pushForm({ staleTimeoutMs: '3600000' }))).toEqual({});
		for (const bad of ['999', '3600001', '1500.5', 'abc']) {
			expect(validatePushFields(pushForm({ staleTimeoutMs: bad })).staleTimeoutMs).toBeDefined();
		}
	});

	it('欄が数として束縛されていても壊れない（`<input type="number">` の bind は数になる）', () => {
		const form = pushForm({ staleTimeoutMs: 60000 as unknown as string });
		expect(validatePushFields(form)).toEqual({});
		expect(pushSettingsFromForm(form)).toEqual({ stale_timeout_ms: 60000 });
	});

	it('ブリッジ: 読む接続・クエリの名前と SQL・列の欄・キーの重複', () => {
		const errors = validatePushFields(
			pushForm({
				bridgeEnabled: true,
				bridgeQueries: [
					{ name: 'a', periodMs: 1000, sql: 'SELECT 1 AS v', columnsText: 'v=k' },
					{ name: 'a', periodMs: 1000, sql: ' ', columnsText: 'v=k' },
					{ name: '', periodMs: 1000, sql: 'SELECT 1', columnsText: 'oops' }
				]
			})
		);
		expect(errors).toEqual({
			bridgeDbConnectionId: 'ブリッジが読む PostgreSQL 接続を選んでください',
			'bridgeQueries[1].name': 'クエリの名前 a が重複しています',
			'bridgeQueries[1].sql': '必須項目です',
			'bridgeQueries[1].columns': 'キー k が区画の中で重複しています',
			'bridgeQueries[2].name': '必須項目です',
			'bridgeQueries[2].columns': '1 行目: 「列名=キー」の形で書いてください'
		});
	});

	it('§7.1: 上限の 8 割を超える設定は保存の前に断る（サーバーと同じ文言）', () => {
		const query = (name: string, periodMs: number) => ({
			name,
			periodMs,
			sql: 'SELECT 1 AS v',
			columnsText: `v=${name}`
		});
		const over = pushForm({
			bridgeEnabled: true,
			bridgeDbConnectionId: '3',
			bridgeQueries: [query('a', 100), query('b', 100), query('c', 100)]
		});
		expect(pushBridgeLoad(over)?.withinLimit).toBe(false);
		expect(validatePushFields(over).bridge).toBe(
			'この設定は 1 秒あたり約 30 メッセージ・30 件になり、上限（16 メッセージ・4,000 件）を超えます。周期を長くするか、クエリを別の push 接続に分けてください'
		);
		const within = pushForm({
			...over,
			bridgeQueries: [query('a', 200), query('b', 200), query('c', 200)]
		});
		expect(pushBridgeLoad(within)?.messagesPerSecText).toBe('15');
		expect(validatePushFields(within)).toEqual({});
	});

	it('pushBridgeLoad はブリッジを持つ push だけ', () => {
		expect(pushBridgeLoad(blankConnectionForm())).toBeNull();
		expect(pushBridgeLoad(pushForm())).toBeNull();
		expect(pushBridgeLoad(connectionToForm(PUSH_CONNECTION))?.queries).toEqual([
			{ items: 2, itemBytes: 10 + 96 + 9 + 96, messagesPerCycle: 1 }
		]);
	});
});

describe('R2a-2e: mapPushFieldErrors（サーバーの settings.* の項目エラーを欄へ写す）', () => {
	it('field の形ごとに欄へ寄せる。他の field はそのまま', () => {
		expect(
			mapPushFieldErrors({
				'settings.stale_timeout_ms': 't',
				'settings.bridge.db_connection_id': 'd',
				'settings.bridge.queries[0].name': 'n',
				'settings.bridge.queries[0].sql': 's',
				'settings.bridge.queries[1].columns[2].key': 'k',
				'settings.bridge.queries[1].period_ms': 'p',
				'settings.bridge': 'load',
				settings: 'size',
				name: 'dup'
			})
		).toEqual({
			staleTimeoutMs: 't',
			bridgeDbConnectionId: 'd',
			'bridgeQueries[0].name': 'n',
			'bridgeQueries[0].sql': 's',
			'bridgeQueries[1].columns': 'k / p',
			bridge: 'load / size',
			name: 'dup'
		});
	});
});

describe('R2a-2e: ingestEndpoints（送り手向けの URL とスコープ）', () => {
	it('接続 id で組み立てる（http → ws、https → wss）', () => {
		expect(ingestEndpoints('http://127.0.0.1:8765', 12)).toEqual({
			restUrl: 'http://127.0.0.1:8765/api/v1/ingest/12/values',
			wsUrl: 'ws://127.0.0.1:8765/api/v1/ingest/12/stream',
			scope: 'ingest:#12'
		});
		expect(ingestEndpoints('https://hub.example/', 3).wsUrl).toBe(
			'wss://hub.example/api/v1/ingest/3/stream'
		);
	});
});

describe('R2a-2e: pushBridgesReferencing（postgres 接続の削除の拒否）', () => {
	it('ブリッジがその id を読む push 接続の名前を返す', () => {
		const other = {
			...PUSH_CONNECTION,
			id: 13,
			name: 'Bridge2',
			settings: { bridge: { ...PUSH_CONNECTION.settings!.bridge!, db_connection_id: 4 } }
		};
		const plain = { ...PUSH_CONNECTION, id: 14, name: 'Script', settings: null };
		const connections = [PUSH_CONNECTION, other, plain];
		expect(pushBridgesReferencing(connections, 3)).toEqual(['Bridge1']);
		expect(pushBridgesReferencing(connections, 4)).toEqual(['Bridge2']);
		expect(pushBridgesReferencing(connections, 5)).toEqual([]);
		expect(formatBridgeReferenceDeleteRefusal('pg-a', ['Bridge1'])).toContain(
			'pg-a は push 接続（Bridge1）の DB ブリッジが読んでいるため削除できません'
		);
	});
});

describe('resolveSavedConnection / isSavedPushConnection（P2: 保存後のプロトコル変更）', () => {
	const conn = (protocol: string): PlcConnection =>
		({ id: 7, name: 'c', protocol }) as unknown as PlcConnection;

	it('保存前は prop のプロトコルが基準（push 以外 → 表示・ポーリングなし）', () => {
		const prop = conn('slmp');
		expect(resolveSavedConnection(prop, null)).toBe(prop);
		expect(isSavedPushConnection(prop, null)).toBe(false);
		expect(isSavedPushConnection(conn('push'), null)).toBe(true);
	});

	it('push 以外 → push を保存すると、prop が古いままでも push 扱い（表示・ポーリング開始）', () => {
		const prop = conn('slmp');
		const saved = conn('push');
		expect(isSavedPushConnection(prop, { base: prop, saved })).toBe(true);
		expect(resolveSavedConnection(prop, { base: prop, saved })).toBe(saved);
	});

	it('push → 他のプロトコルを保存すると、prop が古いままでも push 扱いを外す（停止）', () => {
		const prop = conn('push');
		const saved = conn('slmp');
		expect(isSavedPushConnection(prop, { base: prop, saved })).toBe(false);
	});

	it('未保存のフォーム切替は基準に入らない（引数に form を取らない）', () => {
		// 保存記録が無い限り、判定は prop のみに依存する。
		const prop = conn('slmp');
		expect(isSavedPushConnection(prop, null)).toBe(false);
	});

	it('別の接続を開き直した（prop が別参照）ら古い保存記録は無視する', () => {
		const oldProp = conn('slmp');
		const record = { base: oldProp, saved: conn('push') };
		const newProp = conn('slmp');
		expect(resolveSavedConnection(newProp, record)).toBe(newProp);
		expect(isSavedPushConnection(newProp, record)).toBe(false);
		expect(isSavedPushConnection(null, record)).toBe(false);
	});
});
