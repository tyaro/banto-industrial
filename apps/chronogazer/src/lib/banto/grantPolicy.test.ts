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
	enterCredentiallessGrant,
	enterPublicViewer,
	localDesktopAvailable,
	loginEntries,
	needsFirstAdmin
} from './grantPolicy';

/**
 * `enterGrant(kind)` が `grantable` に含まれる kind のときだけトークンを保存したことに
 * し、次の `resolve()` がその kind のセッションを返す provider。
 */
function grantProvider(status: GrantStatus, grantable: string[]) {
	let revision = 1;
	let held: string | null = null;
	const listeners = new Set<() => void>();
	const rev = () => `${revision}.0` as CredentialRevision;
	const enterGrant = vi.fn(async (kind: string) => {
		if (!grantable.includes(kind)) return { success: false };
		held = kind;
		revision += 1;
		for (const listener of [...listeners]) listener();
		return { success: true };
	});
	const auth: AuthProvider = {
		login: async () => ({ success: true }),
		logout: async () => {},
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
	return { auth, enterGrant };
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
	async function viewer(status: GrantStatus, grantable: string[]) {
		const p = grantProvider(status, grantable);
		initBanto({ dataProvider: {} as DataProvider, authProvider: p.auth, resources: [] });
		const result = await enterPublicViewer(getSessionController(), p.auth);
		return { result, enterGrant: p.enterGrant };
	}

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
