/**
 * `plcConnectionForm.ts`（T18-6a）に対するユニットテスト。`tagFormLayout.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	DEFAULT_PORTS,
	blankConnectionForm,
	connectionToForm,
	defaultPortFor,
	formToConnectionInput,
	isDefaultPortForProtocol,
	nextConnectionName
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

describe('blankConnectionForm / connectionToForm / formToConnectionInput', () => {
	it('blankConnectionForm はバックエンドの既定と一致する初期値を返す', () => {
		expect(blankConnectionForm()).toEqual({
			name: '',
			protocol: 'modbus-tcp',
			host: '',
			port: '502',
			unitId: '1',
			enabled: true,
			simulation: false,
			wordOrder: 'low_high'
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
			wordOrder: 'high_low'
		};
		expect(connectionToForm(conn)).toEqual({
			name: 'Line1',
			protocol: 'slmp',
			host: '192.168.1.10',
			port: '5007',
			unitId: '3',
			enabled: true,
			simulation: false,
			wordOrder: 'high_low'
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
			wordOrder: 'low_high'
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
});
