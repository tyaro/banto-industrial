/**
 * 資格情報なしの口の順序とログイン画面の出し分け（#392 B2-2、`grantPolicy.ts`）。
 * `enterCredentiallessGrant` は本物の `grantFallback`（banto admin-core v6.7.0）と
 * 本物の SessionController の上で動かし、provider の `status()`・`enterGrant()` だけを
 * 差し替える。
 */
import { describe, expect, it, vi } from 'vitest';
import {
	getSessionController,
	initBanto,
	resolveSettled,
	type AuthProvider,
	type CredentialRevision,
	type DataProvider,
	type GrantStatus,
	type ResolvedAuth
} from '@banto/admin-core';
import {
	enteredKind,
	enterCredentiallessGrant,
	enterPublicViewer,
	enterThisPc,
	localDesktopAvailable,
	loginEntries,
	needsFirstAdmin
} from './grantPolicy';

/**
 * `enterGrant(kind)` が `grantable` に含まれる kind のときだけトークンを保存したことに
 * し、次の `resolve()` がその kind のセッションを返す provider。
 */
function grantProvider(status: GrantStatus, grantable: string[], initial: string | null = null) {
	let revision = 1;
	let held: string | null = initial;
	const listeners = new Set<() => void>();
	const rev = () => `${revision}.0` as CredentialRevision;
	const enterGrant = vi.fn(async (kind: string) => {
		if (!grantable.includes(kind)) return { success: false };
		held = kind;
		revision += 1;
		for (const listener of [...listeners]) listener();
		return { success: true };
	});
	// ログアウト: 保存しているセッションを消す（ログイン不要モードの `local` は閉じない -
	// デスクトップの Tauri と同じく断られる形）。
	const logout = vi.fn(async () => {
		if (held === 'local') return;
		held = null;
		revision += 1;
		for (const listener of [...listeners]) listener();
	});
	const auth: AuthProvider = {
		login: async () => ({ success: true }),
		logout,
		resolve: async (): Promise<ResolvedAuth> => {
			const r = rev();
			if (held === null) return { status: 'none', checked: r, current: r };
			return {
				status: 'active',
				checked: r,
				current: r,
				identity: { id: held, name: held, role: 'admin', kind: held }
			};
		},
		credentialRevision: rev,
		onCredentialChanged(listener) {
			listeners.add(listener);
			return () => listeners.delete(listener);
		},
		status: vi.fn(async () => status),
		enterGrant
	};
	return { auth, enterGrant, logout };
}

async function enter(status: GrantStatus, grantable: string[]) {
	const p = grantProvider(status, grantable);
	initBanto({ dataProvider: {} as DataProvider, authProvider: p.auth, resources: [] });
	const controller = getSessionController();
	const first = await resolveSettled(controller);
	if (first.outcome !== 'confirmed') throw new Error('not confirmed');
	const result = await enterCredentiallessGrant(controller, p.auth, first.ticket);
	return { result, enterGrant: p.enterGrant };
}

describe('enterCredentiallessGrant（localDesktop → publicViewer）', () => {
	it('localDesktop が使えれば、閲覧公開も ON でも localDesktop に入る（オーナー決定 2）', async () => {
		const { result, enterGrant } = await enter(
			{ initialized: true, grants: { localDesktop: true, publicViewer: true } },
			['localDesktop', 'publicViewer']
		);
		expect(result.snapshot.status).toBe('active');
		expect(result.snapshot.kind).toBe('localDesktop');
		expect(enterGrant.mock.calls.map((call) => call[0])).toEqual(['localDesktop']);
	});

	it('未初期化でも localDesktop が使えれば入る（オーナー決定 1）', async () => {
		const { result } = await enter({ initialized: false, grants: { localDesktop: true } }, [
			'localDesktop'
		]);
		expect(result.snapshot.kind).toBe('localDesktop');
	});

	it('localDesktop が使えなければ閲覧公開', async () => {
		const { result, enterGrant } = await enter(
			{ initialized: true, grants: { localDesktop: false, publicViewer: true } },
			['publicViewer']
		);
		expect(result.snapshot.kind).toBe('publicViewer');
		expect(enterGrant.mock.calls.map((call) => call[0])).toEqual(['publicViewer']);
	});

	it('localDesktop の発行が断られたら（403）閲覧公開を試す', async () => {
		const { result, enterGrant } = await enter(
			{ initialized: true, grants: { localDesktop: true, publicViewer: true } },
			['publicViewer']
		);
		expect(result.snapshot.kind).toBe('publicViewer');
		expect(enterGrant.mock.calls.map((call) => call[0])).toEqual(['localDesktop', 'publicViewer']);
	});

	it('どちらも使えなければ確定した none のまま（ガードが /login へ）', async () => {
		const { result, enterGrant } = await enter({ initialized: true, grants: {} }, []);
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.status).toBe('none');
		expect(enterGrant).not.toHaveBeenCalled();
	});
});

describe('enterPublicViewer（「閲覧のみで続ける」は明示的に閲覧公開）', () => {
	async function viewer(status: GrantStatus, grantable: string[], initial: string | null = null) {
		const p = grantProvider(status, grantable, initial);
		initBanto({ dataProvider: {} as DataProvider, authProvider: p.auth, resources: [] });
		const result = await enterPublicViewer(getSessionController(), p.auth);
		return { result, enterGrant: p.enterGrant, logout: p.logout };
	}
	const both = { initialized: true, grants: { localDesktop: true, publicViewer: true } };

	it('既に localDesktop のセッション（戻るで来た）なら、ログアウトしてから publicViewer に切り替える', async () => {
		const { result, enterGrant, logout } = await viewer(
			both,
			['localDesktop', 'publicViewer'],
			'localDesktop'
		);
		expect(logout).toHaveBeenCalledTimes(1);
		expect(enterGrant.mock.calls.map((call) => call[0])).toEqual(['publicViewer']);
		expect(enteredKind(result, 'publicViewer')).toBe(true);
	});

	it('既に publicViewer ならそのまま（ログアウトも発行もしない）', async () => {
		const { result, enterGrant, logout } = await viewer(both, ['publicViewer'], 'publicViewer');
		expect(logout).not.toHaveBeenCalled();
		expect(enterGrant).not.toHaveBeenCalled();
		expect(enteredKind(result, 'publicViewer')).toBe(true);
	});

	it('ログイン不要モード（local）は切り替えず、結果は publicViewer ではない（呼び出し側は進まない）', async () => {
		const { result, enterGrant, logout } = await viewer(both, ['publicViewer'], 'local');
		expect(logout).not.toHaveBeenCalled();
		expect(enterGrant).not.toHaveBeenCalled();
		expect(result.snapshot.kind).toBe('local');
		expect(enteredKind(result, 'publicViewer')).toBe(false);
	});

	it('localDesktop も使えるときでも publicViewer に入る（localDesktop を試さない）', async () => {
		const { result, enterGrant } = await viewer(
			{ initialized: true, grants: { localDesktop: true, publicViewer: true } },
			['localDesktop', 'publicViewer']
		);
		expect(result.snapshot.status).toBe('active');
		expect(result.snapshot.kind).toBe('publicViewer');
		expect(enterGrant.mock.calls.map((call) => call[0])).toEqual(['publicViewer']);
	});

	it('閲覧公開が OFF なら none のまま（localDesktop には入らない）', async () => {
		const { result, enterGrant } = await viewer(
			{ initialized: true, grants: { localDesktop: true, publicViewer: false } },
			['localDesktop']
		);
		expect(result.snapshot.status).toBe('none');
		expect(enterGrant).not.toHaveBeenCalled();
	});
});

describe('enterThisPc（「この PC として続ける」も明示的に選ぶ）', () => {
	it('既に publicViewer なら、ログアウトしてから localDesktop に切り替える', async () => {
		const p = grantProvider(
			{ initialized: true, grants: { localDesktop: true, publicViewer: true } },
			['localDesktop', 'publicViewer'],
			'publicViewer'
		);
		initBanto({ dataProvider: {} as DataProvider, authProvider: p.auth, resources: [] });
		const result = await enterThisPc(getSessionController(), p.auth);
		expect(p.logout).toHaveBeenCalledTimes(1);
		expect(p.enterGrant.mock.calls.map((call) => call[0])).toEqual(['localDesktop']);
		expect(enteredKind(result, 'localDesktop')).toBe(true);
	});

	it('localDesktop が使えなければ none（呼び出し側は進まない）', async () => {
		const p = grantProvider({ initialized: true, grants: { publicViewer: true } }, [
			'localDesktop',
			'publicViewer'
		]);
		initBanto({ dataProvider: {} as DataProvider, authProvider: p.auth, resources: [] });
		const result = await enterThisPc(getSessionController(), p.auth);
		expect(p.enterGrant).not.toHaveBeenCalled();
		expect(enteredKind(result, 'localDesktop')).toBe(false);
	});
});

describe('enteredKind（呼び出し側が進むか）', () => {
	const active = (kind: string) =>
		({
			outcome: 'confirmed',
			snapshot: { status: 'active', kind },
			ticket: {}
		}) as unknown as Parameters<typeof enteredKind>[0];
	it('選んだ kind の確定した active だけ真', () => {
		expect(enteredKind(active('publicViewer'), 'publicViewer')).toBe(true);
		expect(enteredKind(active('localDesktop'), 'publicViewer')).toBe(false);
		expect(enteredKind(active('account'), 'publicViewer')).toBe(false);
		expect(
			enteredKind(
				{ outcome: 'unverified', error: null, snapshot: {} } as unknown as Parameters<
					typeof enteredKind
				>[0],
				'publicViewer'
			)
		).toBe(false);
	});
});

describe('localDesktopAvailable・needsFirstAdmin', () => {
	it('grants.localDesktop が true のときだけ使える（無い・false・status 無しは使えない）', () => {
		expect(localDesktopAvailable({ initialized: true, grants: { localDesktop: true } })).toBe(true);
		expect(localDesktopAvailable({ initialized: true, grants: { localDesktop: false } })).toBe(
			false
		);
		expect(localDesktopAvailable({ initialized: true })).toBe(false);
		expect(localDesktopAvailable(undefined)).toBe(false);
	});

	it('案内の帯は localDesktop のセッションで未初期化と読めたときだけ', () => {
		expect(needsFirstAdmin(true, { initialized: false })).toBe(true);
		expect(needsFirstAdmin(true, { initialized: true })).toBe(false);
		expect(needsFirstAdmin(true, undefined)).toBe(false);
		expect(needsFirstAdmin(false, { initialized: false })).toBe(false);
	});
});

describe('loginEntries（ログイン画面の出し分け）', () => {
	const none = { continueAsThisPc: false, continueAsViewer: false, localGrantOptIn: false };
	const cases: [string, 'tauri' | 'server' | 'demo', GrantStatus | undefined, typeof none][] = [
		[
			'server・初期化済み・localDesktop → この PC として続ける',
			'server',
			{ initialized: true, grants: { localDesktop: true } },
			{ ...none, continueAsThisPc: true }
		],
		[
			'server・未初期化・localDesktop → 初回セットアップの選択',
			'server',
			{ initialized: false, grants: { localDesktop: true } },
			{ ...none, localGrantOptIn: true }
		],
		[
			'server・未初期化・localDesktop なし（LAN の画面）→ 選択を出さない',
			'server',
			{ initialized: false, grants: { localDesktop: false } },
			none
		],
		[
			'server・閲覧公開 ON → 閲覧のみで続ける',
			'server',
			{ initialized: true, grants: { publicViewer: true } },
			{ ...none, continueAsViewer: true }
		],
		[
			'server・両方',
			'server',
			{ initialized: true, grants: { localDesktop: true, publicViewer: true } },
			{ continueAsThisPc: true, continueAsViewer: true, localGrantOptIn: false }
		],
		['server・status が読めない', 'server', undefined, none],
		[
			'tauri は何も出さない',
			'tauri',
			{ initialized: false, grants: { localDesktop: true, publicViewer: true } },
			none
		],
		['demo は何も出さない', 'demo', { initialized: true, grants: { localDesktop: true } }, none]
	];
	for (const [label, mode, status, expected] of cases) {
		it(label, () => {
			expect(loginEntries(mode, status)).toEqual(expected);
		});
	}
});
