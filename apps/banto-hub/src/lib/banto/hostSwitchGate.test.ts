/**
 * `hostSwitchGate.ts` のユニットテスト。
 */
import { describe, expect, it } from 'vitest';
import {
	canRestoreSettingMode,
	canSwitchToDesktop,
	canSwitchToService,
	canToggleAutostart,
	fallbackNotice,
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

describe('fallbackNotice（#661 661-5）', () => {
	it('締まったまま止まった（5）は「運用モードのまま残っています」と [設定モードに戻す]', () => {
		const notice = fallbackNotice('leftTight');
		expect(notice?.title).toBe('運用モードのまま残っています');
		expect(notice?.message).toContain('終了コード 5');
		expect(notice?.offerRestore).toBe(true);
	});

	it('運用モード（停止中）も戻せる', () => {
		expect(fallbackNotice('operationStopped')?.offerRestore).toBe(true);
	});

	it('4・使えない・止まっただけは戻す操作を出さない', () => {
		const cannot = fallbackNotice('cannotOperate');
		expect(cannot?.title).toBe('運用モードにできませんでした');
		expect(cannot?.message).toContain('設定モードのまま');
		expect(cannot?.offerRestore).toBe(false);
		expect(fallbackNotice('profileUnusable')?.offerRestore).toBe(false);
		expect(fallbackNotice('serviceLost')?.offerRestore).toBe(false);
	});

	it('一般・知らない名前は null', () => {
		expect(fallbackNotice(null)).toBeNull();
		expect(fallbackNotice(undefined)).toBeNull();
		expect(fallbackNotice('somethingNew')).toBeNull();
	});
});

describe('canRestoreSettingMode（#661 661-5）', () => {
	it('ローカルシェルで締まったままなら、Operators でなくても押せる', () => {
		expect(canRestoreSettingMode({ ...base, canOperate: false }, 'leftTight')).toBe(true);
	});

	it('切替中・非シェル・戻す理由でないときは押せない', () => {
		expect(canRestoreSettingMode({ ...base, switching: true }, 'leftTight')).toBe(false);
		expect(canRestoreSettingMode({ ...base, isLocalShell: false }, 'leftTight')).toBe(false);
		expect(canRestoreSettingMode(base, 'cannotOperate')).toBe(false);
		expect(canRestoreSettingMode(base, null)).toBe(false);
	});
});
