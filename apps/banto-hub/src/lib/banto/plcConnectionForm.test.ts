/**
 * `plcConnectionForm.ts`（T18-6a）に対するユニットテスト。`tagFormLayout.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	DEFAULT_POSTGRES_PORT,
	blankConnectionForm,
	connectionToForm,
	driverFor,
	driverSettingsForSubmit,
	driverStateAfterProtocolChange,
	rebuiltDriverState,
	formToConnectionInput,
	nextConnectionName,
	passwordForSubmit,
	protocolOptions,
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
import { TEST_DRIVERS } from './configPackageTestDrivers';
import { isFormDirty } from './formDirty';
import type { PlcConnection, PushSettingsWire } from './tagRegistryAdmin';

const DRIVERS = TEST_DRIVERS;
const blank = () => blankConnectionForm(DRIVERS);

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

describe('protocolOptions / driverFor（6c: 選択肢は GET /api/drivers + ホストの接続）', () => {
	it('ドライバ（displayName）の後に postgres・push が並ぶ。virtual は出ない', () => {
		const options = protocolOptions([
			{ id: 'modbus-tcp', displayName: 'Modbus TCP' },
			{ id: 'slmp', displayName: 'SLMP（MELSEC）' }
		]);
		expect(options).toEqual([
			{ value: 'modbus-tcp', label: 'Modbus TCP' },
			{ value: 'slmp', label: 'SLMP（MELSEC）' },
			{ value: 'postgres', label: 'PostgreSQL（DB Source）' },
			{ value: 'push', label: '取り込み（push・送り手が値を送る）' }
		]);
	});

	it('ドライバが 0 件でもホストの接続は残る', () => {
		expect(protocolOptions([]).map((o) => o.value)).toEqual(['postgres', 'push']);
	});

	it('driverFor: ドライバは引け、ホストの接続・未知は undefined', () => {
		expect(driverFor(DRIVERS, 'slmp')?.id).toBe('slmp');
		expect(driverFor(DRIVERS, 'postgres')).toBeUndefined();
		expect(driverFor(DRIVERS, 'virtual')).toBeUndefined();
	});
});

describe('driverStateAfterProtocolChange（6c: プロトコルを替えたときの引き継ぎ）', () => {
	it('modbus → slmp: 触ったホストだけ引き継ぎ、触っていないポート・ワード順は新しい既定', () => {
		const form = blank();
		form.driver.values.host = '10.0.0.9';
		form.driver.touched.host = true;
		const next = driverStateAfterProtocolChange(DRIVERS, 'modbus-tcp', 'slmp', form.driver);
		expect(next.values).toEqual({
			host: '10.0.0.9',
			port: '5007',
			cpu_type: 'R',
			word_order: 'low_high'
		});
	});

	it('触ったポートは引き継ぐ', () => {
		const form = blank();
		form.driver.values.port = '1502';
		form.driver.touched.port = true;
		const next = driverStateAfterProtocolChange(DRIVERS, 'modbus-tcp', 'slmp', form.driver);
		expect(next.values.port).toBe('1502');
	});

	it('ホストの接続 → ドライバは既定で埋める（古いドライバの欄は持ち越さない）', () => {
		const stale = blank().driver;
		stale.values.host = 'leftover';
		stale.touched.host = true;
		const next = driverStateAfterProtocolChange(DRIVERS, 'postgres', 'slmp', stale);
		expect(next.values.host).toBe('');
		expect(next.values.port).toBe('5007');
		expect(next.touched).toEqual({});
	});

	it('ドライバ → ホストの接続は状態をそのまま返す', () => {
		const state = blank().driver;
		expect(driverStateAfterProtocolChange(DRIVERS, 'modbus-tcp', 'postgres', state)).toBe(state);
	});
});

describe('blankConnectionForm / connectionToForm / formToConnectionInput', () => {
	it('blankConnectionForm の既定プロトコルは drivers の先頭で、欄はそのスキーマの既定', () => {
		expect(blank()).toEqual({
			name: '',
			protocol: 'modbus-tcp',
			host: '',
			port: '5432',
			enabled: true,
			simulation: false,
			driver: {
				values: { host: '', port: '502', unit_id: '1', word_order: 'high_low' },
				touched: {}
			},
			database: '',
			username: '',
			password: '',
			clearPassword: false,
			passwordState: 'none',
			staleTimeoutMs: '',
			bridgeEnabled: false,
			bridgeDbConnectionId: '',
			bridgeQueries: [],
			savedSettings: null
		});
		expect(DEFAULT_POSTGRES_PORT).toBe(5432);
	});

	it('drivers が空でも壊れない（プロトコルは空）', () => {
		const form = blankConnectionForm([]);
		expect(form.protocol).toBe('');
		expect(form.driver.values).toEqual({});
	});

	it('connectionToForm はドライバの settings を文字列化し、無い欄は既定にする', () => {
		const conn: PlcConnection = {
			id: 7,
			name: 'Line1',
			protocol: 'slmp',
			enabled: true,
			simulation: false,
			settings: { host: '192.168.1.10', port: 5007, cpu_type: 'Q' }
		};
		const form = connectionToForm(conn, DRIVERS);
		expect(form.protocol).toBe('slmp');
		expect(form.driver.values).toEqual({
			host: '192.168.1.10',
			port: '5007',
			cpu_type: 'Q',
			word_order: 'low_high'
		});
		expect(form.database).toBe('');
	});

	it('formToConnectionInput はドライバの settings を snake_case のまま・数値は number で送る（往復）', () => {
		const conn: PlcConnection = {
			id: 1,
			name: 'X',
			protocol: 'modbus-tcp',
			enabled: true,
			simulation: true,
			settings: { host: 'h', port: 1502, unit_id: 3, word_order: 'low_high' }
		};
		expect(formToConnectionInput(connectionToForm(conn, DRIVERS), DRIVERS)).toEqual({
			name: 'X',
			protocol: 'modbus-tcp',
			enabled: true,
			simulation: true,
			settings: { host: 'h', port: 1502, unit_id: 3, word_order: 'low_high' }
		});
	});

	const PG: PlcConnection = {
		id: 9,
		name: 'pg1',
		protocol: 'postgres',
		enabled: true,
		simulation: false,
		settings: {
			host: '127.0.0.1',
			port: 5432,
			database: 'appdb',
			username: 'appuser',
			password: { $secret: 'set' }
		}
	};

	it('S1: connectionToForm は postgres の settings を読み、パスワードは決してプリフィルしない（$secret は状態だけ）', () => {
		const form = connectionToForm(PG, DRIVERS);
		expect(form.password).toBe('');
		expect(form.clearPassword).toBe(false);
		expect(form.passwordState).toBe('set');
		expect(form.host).toBe('127.0.0.1');
		expect(form.port).toBe('5432');
		expect(form.database).toBe('appdb');
		expect(form.username).toBe('appuser');
	});

	it.each([
		['set', 'set'],
		['unreadable', 'unreadable']
	] as const)('postgres の password $secret=%s は passwordState=%s', (wire, state) => {
		const form = connectionToForm(
			{ ...PG, settings: { ...PG.settings, password: { $secret: wire } } },
			DRIVERS
		);
		expect(form.passwordState).toBe(state);
	});

	it('postgres の password が載っていなければ passwordState は none', () => {
		const rest = { ...PG.settings };
		delete rest.password;
		expect(connectionToForm({ ...PG, settings: rest }, DRIVERS).passwordState).toBe('none');
	});

	it('S1: formToConnectionInput の postgres は settings に host・port・database・username（と入力したパスワード）を入れる', () => {
		const form: PlcConnectionFormState = {
			...blank(),
			protocol: 'postgres',
			host: ' 127.0.0.1 ',
			port: '5432',
			database: 'appdb',
			username: 'appuser',
			password: 'hunter2',
			// ドライバの欄が残っていても送らない。
			simulation: true
		};
		expect(formToConnectionInput(form, DRIVERS)).toEqual({
			name: '',
			protocol: 'postgres',
			enabled: true,
			simulation: false,
			settings: {
				host: '127.0.0.1',
				port: 5432,
				database: 'appdb',
				username: 'appuser',
				password: 'hunter2'
			}
		});
	});

	it('postgres のパスワード: 未入力は password キーを送らない、消去は null', () => {
		const base: PlcConnectionFormState = { ...connectionToForm(PG, DRIVERS) };
		expect(formToConnectionInput(base, DRIVERS).settings).not.toHaveProperty('password');
		expect(
			formToConnectionInput({ ...base, clearPassword: true }, DRIVERS).settings.password
		).toBeNull();
	});

	it('S1: ドライバの接続は postgres の項目（database/username/password）を一切送らない', () => {
		const form: PlcConnectionFormState = {
			...blank(),
			protocol: 'modbus-tcp',
			// postgres → modbus と切り替えた直後の残留値。
			database: 'leftover-db',
			username: 'leftover-user',
			password: 'leftover-pw'
		};
		const input = formToConnectionInput(form, DRIVERS);
		expect(input.settings).not.toHaveProperty('database');
		expect(input.settings).not.toHaveProperty('username');
		expect(input.settings).not.toHaveProperty('password');
	});

	it('秘密の欄を持つドライバ: 保つ=キー無し、消す=null', () => {
		const conn: PlcConnection = {
			id: 2,
			name: 's',
			protocol: 'fake-secret',
			enabled: true,
			simulation: false,
			settings: { host: 'h', token: { $secret: 'set' } }
		};
		const form = connectionToForm(conn, DRIVERS);
		expect(formToConnectionInput(form, DRIVERS).settings).toEqual({ host: 'h', note: '' });
		const cleared = structuredClone(form);
		(cleared.driver.values.token as { mode: string }).mode = 'clear';
		expect(formToConnectionInput(cleared, DRIVERS).settings).toEqual({
			host: 'h',
			token: null,
			note: ''
		});
	});
});

describe('S1: passwordForSubmit（パスワード tri-state の組み立て）', () => {
	it('clearPassword が true なら password の中身に関わらず null を返す（消去）', () => {
		expect(passwordForSubmit({ password: 'ignored', clearPassword: true })).toBeNull();
		expect(passwordForSubmit({ password: '', clearPassword: true })).toBeNull();
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
		const baseline = blank();
		const form = { ...baseline };
		expect(isFormDirty(baseline, form)).toBe(false);
	});

	it('新規作成: 名前を1文字変えると dirty になる', () => {
		const baseline = blank();
		const form = { ...baseline, name: 'a' };
		expect(isFormDirty(baseline, form)).toBe(true);
	});

	it('再設定: 開いた直後（baseline = connectionToForm(connection)）は dirty ではない', () => {
		const conn: PlcConnection = {
			id: 7,
			name: 'Line1',
			protocol: 'slmp',
			enabled: true,
			simulation: false,
			settings: { host: '192.168.1.10', port: 5007 }
		};
		const baseline = connectionToForm(conn, DRIVERS);
		const form = { ...baseline };
		expect(isFormDirty(baseline, form)).toBe(false);
	});

	it('再設定: ホストを1文字変えると dirty になる', () => {
		const conn: PlcConnection = {
			id: 7,
			name: 'Line1',
			protocol: 'slmp',
			enabled: true,
			simulation: false,
			settings: { host: '192.168.1.10', port: 5007 }
		};
		const baseline = connectionToForm(conn, DRIVERS);
		const form = structuredClone(baseline);
		form.driver.values.host = '192.168.1.11';
		expect(isFormDirty(baseline, form)).toBe(true);
	});

	it('readOnly: 入力欄が disabled で form が変わらない前提なら、baseline と同一のまま dirty にならない（readOnly 専用の分岐は無く isFormDirty だけで足りる）', () => {
		const conn: PlcConnection = {
			id: 9,
			name: 'ViewOnly',
			protocol: 'modbus-tcp',
			enabled: true,
			simulation: false,
			settings: { host: '10.0.0.1', port: 502 }
		};
		const baseline = connectionToForm(conn, DRIVERS);
		// readOnly では <input disabled> によりユーザーは form を変更できない
		// - つまり form は常に baseline と同一のまま。
		const form = baseline;
		expect(isFormDirty(baseline, form)).toBe(false);
	});
});

// --- R2a-2e: 取り込み口 push（docs/banto-hub-ingest-design.md §3.2・§7.1） ---

function pushForm(overrides: Partial<PlcConnectionFormState> = {}): PlcConnectionFormState {
	return {
		...blank(),
		name: 'Bridge1',
		protocol: 'push',
		...overrides
	};
}

const PUSH_SETTINGS: PushSettingsWire = {
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
};

const PUSH_CONNECTION: PlcConnection = {
	id: 12,
	name: 'Bridge1',
	protocol: 'push',
	enabled: true,
	simulation: false,
	settings: PUSH_SETTINGS as unknown as Record<string, unknown>
};

describe('R2a-2e: push の settings とフォームの往復', () => {
	it('connectionToForm → formToConnectionInput で settings がそのまま戻る', () => {
		const form = connectionToForm(PUSH_CONNECTION, DRIVERS);
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
		const input = formToConnectionInput(form, DRIVERS);
		expect(input.settings).toEqual(PUSH_SETTINGS);
		// push は接続先を持たない。postgres の項目も送らない。
		expect(input.simulation).toBe(false);
		expect(input.settings).not.toHaveProperty('host');
		expect(input.settings).not.toHaveProperty('password');
	});

	it('既定のまま（settings: null）の push は空の settings を送る（update で保存済みの値を消せるように）', () => {
		const form = connectionToForm({ ...PUSH_CONNECTION, settings: {} }, DRIVERS);
		expect(form.staleTimeoutMs).toBe('');
		expect(form.bridgeEnabled).toBe(false);
		expect(formToConnectionInput(form, DRIVERS).settings).toEqual({});
	});

	it('ブリッジを外すと settings から bridge が消える', () => {
		const form = { ...connectionToForm(PUSH_CONNECTION, DRIVERS), bridgeEnabled: false };
		expect(pushSettingsFromForm(form)).toEqual({ stale_timeout_ms: 5000 });
	});

	it('push 以外の接続に push の settings は混ざらない', () => {
		const settings = formToConnectionInput(blank(), DRIVERS).settings;
		expect(settings).not.toHaveProperty('stale_timeout_ms');
		expect(settings).not.toHaveProperty('bridge');
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
		expect(validatePushFields(blank())).toEqual({});
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
		expect(pushBridgeLoad(blank())).toBeNull();
		expect(pushBridgeLoad(pushForm())).toBeNull();
		expect(pushBridgeLoad(connectionToForm(PUSH_CONNECTION, DRIVERS))?.queries).toEqual([
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
			settings: { bridge: { ...PUSH_SETTINGS.bridge!, db_connection_id: 4 } }
		};
		const plain = { ...PUSH_CONNECTION, id: 14, name: 'Script', settings: {} };
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
	const conn = (protocol: string): PlcConnection => ({
		id: 7,
		name: 'c',
		protocol,
		enabled: true,
		simulation: false,
		settings: {}
	});

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

describe('driverSettingsForSubmit / rebuiltDriverState（スキーマが分からない保存）', () => {
	const saved: PlcConnection = {
		id: 1,
		name: 'X',
		protocol: 'modbus-tcp',
		enabled: true,
		simulation: false,
		settings: { host: 'h', port: 1502, token: { $secret: 'set' } }
	};
	const none: readonly never[] = [];

	it.each([
		[
			'スキーマ無し + 保存済み: そのまま（$secret は落とす）',
			saved,
			none,
			{ host: 'h', port: 1502 }
		],
		[
			'スキーマ無し + 一覧に無いプロトコル',
			{ ...saved, protocol: 'virtual', settings: { a: 1 } },
			DRIVERS,
			{ a: 1 }
		],
		['スキーマ無し + 新規: {}', null, none, {}],
		['スキーマあり: buildDriverSettings', saved, DRIVERS, { host: 'h', port: 1502 }]
	])('%s', (_name, conn, drivers, expected) => {
		const form = conn ? connectionToForm(conn, drivers) : { ...blank(), protocol: 'modbus-tcp' };
		const out = driverSettingsForSubmit(form, drivers);
		if (drivers === DRIVERS) expect(out).toMatchObject(expected);
		else expect(out).toEqual(expected);
	});

	it('postgres・push は今どおり（formToConnectionInput が欄から組む）', () => {
		const pg = connectionToForm(
			{
				...saved,
				protocol: 'postgres',
				settings: { host: 'p', port: 5432, database: 'd', username: 'u' }
			},
			none
		);
		expect(formToConnectionInput(pg, none).settings).toMatchObject({ host: 'p', database: 'd' });
		const push = connectionToForm({ ...saved, protocol: 'push', settings: {} }, none);
		expect(formToConnectionInput(push, none).settings).not.toHaveProperty('token');
	});

	it('rebuiltDriverState: 同じプロトコルで欄が空のときだけ保存済みから作り直す', () => {
		const form = connectionToForm(saved, none);
		const rebuilt = rebuiltDriverState(form, 'modbus-tcp', DRIVERS);
		expect(rebuilt?.values.host).toBe('h');
		expect(rebuiltDriverState({ ...form, protocol: 'slmp' }, 'modbus-tcp', DRIVERS)).toBeNull();
		expect(rebuiltDriverState(blank(), 'modbus-tcp', DRIVERS)).toBeNull();
		expect(rebuiltDriverState(connectionToForm(saved, DRIVERS), 'modbus-tcp', DRIVERS)).toBeNull();
	});
});
