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
	type PlcConnectionFormState
} from './plcConnectionForm';
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
			database: '',
			username: '',
			password: '',
			clearPassword: false
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
			database: '',
			username: '',
			password: '',
			clearPassword: false
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
			wordOrder: 'low_high'
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
