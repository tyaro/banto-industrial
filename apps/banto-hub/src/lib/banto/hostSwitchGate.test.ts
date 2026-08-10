/**
 * `hostSwitchGate.ts` のユニットテスト。
 */
import { describe, expect, it } from 'vitest';
import {
	canSwitchToDesktop,
	canSwitchToService,
	canToggleAutostart,
	hostSwitchDisabledReason,
	isPreflightOk,
	type HostSwitchGateInput
} from './hostSwitchGate';

const base: HostSwitchGateInput = {
	isLocalShell: true,
	isAdmin: true,
	canOperate: true,
	view: 'desktop',
	switching: false,
	lastConfigError: null,
	hasRevision: true
};

describe('isPreflightOk', () => {
	it('revision があり設定エラーが無ければ true', () => {
		expect(isPreflightOk({ lastConfigError: null, hasRevision: true })).toBe(true);
	});

	it('設定エラーがあると false', () => {
		expect(isPreflightOk({ lastConfigError: 'bad tag', hasRevision: true })).toBe(false);
	});

	it('revision が無いと false', () => {
		expect(isPreflightOk({ lastConfigError: null, hasRevision: false })).toBe(false);
	});
});

describe('canSwitchToService', () => {
	it('Desktop で権限と preflight が揃えば true', () => {
		expect(canSwitchToService(base)).toBe(true);
	});

	it('fallback からも開始できる', () => {
		expect(canSwitchToService({ ...base, view: 'fallback' })).toBe(true);
	});

	it('既に service なら false', () => {
		expect(canSwitchToService({ ...base, view: 'service' })).toBe(false);
	});

	it('非シェル・非 admin・非 operate・切替中は false', () => {
		expect(canSwitchToService({ ...base, isLocalShell: false })).toBe(false);
		expect(canSwitchToService({ ...base, isAdmin: false })).toBe(false);
		expect(canSwitchToService({ ...base, canOperate: false })).toBe(false);
		expect(canSwitchToService({ ...base, switching: true })).toBe(false);
	});
});

describe('canSwitchToDesktop', () => {
	it('service ビューでのみ true', () => {
		expect(canSwitchToDesktop({ ...base, view: 'service' })).toBe(true);
		expect(canSwitchToDesktop(base)).toBe(false);
	});
});

describe('canToggleAutostart', () => {
	it('シェル＋権限があれば view に依らず true（切替中は false）', () => {
		expect(canToggleAutostart(base)).toBe(true);
		expect(canToggleAutostart({ ...base, view: 'service' })).toBe(true);
		expect(canToggleAutostart({ ...base, switching: true })).toBe(false);
	});
});

describe('hostSwitchDisabledReason', () => {
	it('非シェル時はローカルシェルが必要と返す', () => {
		expect(hostSwitchDisabledReason({ ...base, isLocalShell: false })).toMatch(/ローカルシェル/);
	});

	it('操作可能なら null', () => {
		expect(hostSwitchDisabledReason(base)).toBeNull();
	});
});
