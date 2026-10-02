/**
 * 試運転モードの policy runner（`commissioningPolicy.ts`、banto v2.0.0 #260、
 * 設計 §6.2）のテスト。
 *
 * 1. 1 回分の判断 {@link decideCommissioningStep} を、mode × 試運転の状態 ×
 *    合成セッションの有無の**総当たりの表**で固定する（実装チェックリスト §5
 *    「判断は純関数に出して、状態の総当たりを表でテストする」）。
 * 2. runner を**本物の** SessionController（`createSessionController`）と偽の
 *    provider の上で走らせ、adopt/end・ticket の失効のやり直し・期限・上限を
 *    確かめる（S-44・S-45・S-53・S-62・S-70・S-71）。
 * 3. 期限で見捨てるとき、**本物の** HTTP ヘルパー（`fetchCommissioningStatusOrNull`）
 *    に渡した signal で `/api/commissioning/status` の要求そのものが止まる
 *    （チェックリスト §5「時間切れで見捨てた非同期処理の副作用」。取得関数を
 *    丸ごと差し替えると見えない）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	createSessionController,
	initBanto,
	resolveSettled,
	type DataProvider,
	type SessionController
} from '@banto/admin-core';

vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));

import { COMMISSIONING_IDENTITY, type CommissioningStatus } from './commissioning';
import {
	COMMISSIONING_KIND,
	COMMISSIONING_POLICY_DEADLINE_MS,
	decideCommissioningStep,
	isCommissioningSession,
	PolicyExhaustedError,
	PolicyStatusUnavailableError,
	PolicyTimeoutError,
	runCommissioningPolicy,
	type CommissioningPolicyMode,
	type CommissioningStep
} from './commissioningPolicy';
import { accountAnswer, ALICE, fakeAuthProvider, noneAnswer } from './testing/fakeAuthProvider';

const UNLOCKED: CommissioningStatus = { lockedDown: false };
const LOCKED: CommissioningStatus = { lockedDown: true };

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('decideCommissioningStep（1 回分の判断、総当たり）', () => {
	const cases: Array<
		[CommissioningPolicyMode, CommissioningStatus | null, boolean, CommissioningStep]
	> = [
		// guard: 取得の失敗は「迂回しない」= ロックダウン済みと同じ（v1 の +layout.ts と同じ安全側）
		['guard', UNLOCKED, false, 'adopt'],
		['guard', UNLOCKED, true, 'adopt'], // 同じ C の再 adopt（generation 据え置き、S-44）
		['guard', LOCKED, false, 'handOff'],
		['guard', LOCKED, true, 'end'],
		['guard', null, false, 'handOff'],
		['guard', null, true, 'end'],
		// recheck: 取得の失敗は終了の確定ではない（S-71）。end も adopt もしない
		['recheck', UNLOCKED, false, 'adopt'],
		['recheck', UNLOCKED, true, 'adopt'],
		['recheck', LOCKED, false, 'handOff'],
		['recheck', LOCKED, true, 'end'],
		['recheck', null, false, 'statusUnavailable'],
		['recheck', null, true, 'statusUnavailable']
	];
	it.each(cases)('%s・状態 %j・試運転のセッション %s → %s', (mode, status, live, expected) => {
		expect(decideCommissioningStep(mode, status, live)).toBe(expected);
	});
});

/** 偽の provider に結び付けた本物の controller。`fetchStatus` は呼ばれた回数と signal を記録する。 */
function setup(answer = noneAnswer) {
	const provider = fakeAuthProvider(answer);
	const controller = createSessionController(provider.auth);
	const signals: AbortSignal[] = [];
	let status: CommissioningStatus | null | (() => Promise<CommissioningStatus | null>) = UNLOCKED;
	const fetchStatus = vi.fn(async (signal: AbortSignal) => {
		signals.push(signal);
		return typeof status === 'function' ? status() : status;
	});
	return {
		provider,
		controller,
		signals,
		fetchStatus,
		setStatus(next: typeof status): void {
			status = next;
		},
		run(mode: CommissioningPolicyMode, options: { deadlineMs?: number; maxRounds?: number } = {}) {
			return runCommissioningPolicy(controller, { mode, ...options }, { fetchStatus });
		}
	};
}

/** 試運転の合成セッションを確定させた状態にする（guard を 1 回通す）。 */
async function adopted(s: ReturnType<typeof setup>): Promise<number> {
	s.setStatus(UNLOCKED);
	const result = await s.run('guard');
	expect(result.outcome).toBe('confirmed');
	expect(isCommissioningSession(s.controller.snapshot)).toBe(true);
	return s.controller.snapshot.generation;
}

describe('runCommissioningPolicy: guard（ルートガード）', () => {
	it('S-44: 試運転モードなら合成セッションを adopt で確定する。provider には問い合わせない', async () => {
		const s = setup();
		const before = s.controller.snapshot.generation;
		const result = await s.run('guard');

		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.status).toBe('active');
		expect(result.snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(result.snapshot.identity).toEqual(COMMISSIONING_IDENTITY);
		expect(result.snapshot.owner).toBe('commissioning:commissioning');
		expect(result.snapshot.generation).toBe(before + 1);
		expect(s.provider.resolve).not.toHaveBeenCalled();
		// 方針の通信には runner 自身の signal を渡している。
		expect(s.signals).toHaveLength(1);
		expect(s.signals[0]).toBeInstanceOf(AbortSignal);
	});

	it('S-44: 同じ C の再 adopt（毎回の load）は generation を据え置く = 世代ゲートが画面を作り直さない', async () => {
		const s = setup();
		const generation = await adopted(s);
		for (let i = 0; i < 3; i++) {
			const result = await s.run('guard');
			expect(result.outcome).toBe('confirmed');
			expect(result.snapshot.generation).toBe(generation);
		}
		expect(s.provider.resolve).not.toHaveBeenCalled();
	});

	it('ロックダウン済みで試運転でなければ、通常の確認（resolveSettled）へ引き継ぐ', async () => {
		const s = setup(accountAnswer(ALICE));
		s.setStatus(LOCKED);
		const result = await s.run('guard');
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.status).toBe('active');
		expect(result.snapshot.kind).toBe('account');
		expect(s.provider.resolve).toHaveBeenCalledTimes(1);
	});

	it('状態を取得できなければ迂回しない（ロックダウン済みと同じ。トークンが無ければ none）', async () => {
		const s = setup(noneAnswer);
		s.setStatus(null);
		const result = await s.run('guard');
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.status).toBe('none');
		expect(isCommissioningSession(s.controller.snapshot)).toBe(false);
	});

	it('試運転の合成セッションの後にロックダウンを確認したら end してから通常の確認', async () => {
		const s = setup(noneAnswer);
		const generation = await adopted(s);
		s.setStatus(LOCKED);
		const result = await s.run('guard');
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.status).toBe('none');
		expect(result.snapshot.generation).toBeGreaterThan(generation);
		expect(s.provider.resolve).toHaveBeenCalled();
	});

	it('guard で取得に失敗したら、試運転の合成セッションも end する（迂回しない側。v1 と同じ）', async () => {
		const s = setup(noneAnswer);
		await adopted(s);
		s.setStatus(null);
		const result = await s.run('guard');
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.status).toBe('none');
	});

	it('S-53: 状態を待つ間に資格情報が変わって ticket が失効したら、新しい ticket で確認からやり直す', async () => {
		const s = setup(noneAnswer);
		let first = true;
		s.setStatus(async () => {
			if (first) {
				first = false;
				s.provider.change(); // 別タブのログイン（storage イベント）。adopt 前なので ticket は revision を持つ
			}
			return UNLOCKED;
		});
		const result = await s.run('guard');
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(s.fetchStatus).toHaveBeenCalledTimes(2);
	});

	it('S-70: やり直しが上限に達したら unverified（PolicyExhaustedError）。C を confirmed にしない', async () => {
		const s = setup(noneAnswer);
		s.setStatus(async () => {
			s.provider.change(); // 毎回失効させる
			return UNLOCKED;
		});
		const result = await s.run('guard', { maxRounds: 3 });
		expect(result.outcome).toBe('unverified');
		if (result.outcome === 'unverified') expect(result.error).toBeInstanceOf(PolicyExhaustedError);
		expect(s.fetchStatus).toHaveBeenCalledTimes(3);
		expect(isCommissioningSession(s.controller.snapshot)).toBe(false);
	});

	it('S-62: adopt 中の ticket は revision を持たないので、別タブのログインでは end が失効しない', async () => {
		const s = setup(accountAnswer(ALICE));
		await adopted(s);
		s.setStatus(async () => {
			s.provider.change(); // adopt 中の storage イベント: 保留にせず pendingBackground だけ（S-46）
			return LOCKED;
		});
		const result = await s.run('guard');
		expect(s.fetchStatus).toHaveBeenCalledTimes(2); // adopted の 1 回 + この 1 回（やり直し無し）
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.kind).toBe('account'); // end の後の確認で A が確定
	});
});

describe('runCommissioningPolicy: recheck（再接続の失敗後の確認）', () => {
	it('S-71: 状態を取得できなければ unverified。試運転の合成セッションは end しない（画面を保つ）', async () => {
		const s = setup(noneAnswer);
		const generation = await adopted(s);
		s.setStatus(null);
		const result = await s.run('recheck');
		expect(result.outcome).toBe('unverified');
		if (result.outcome === 'unverified') {
			expect(result.error).toBeInstanceOf(PolicyStatusUnavailableError);
		}
		expect(isCommissioningSession(s.controller.snapshot)).toBe(true);
		expect(s.controller.snapshot.generation).toBe(generation);
		expect(s.provider.resolve).not.toHaveBeenCalled();
	});

	it('試運転のままなら session（同じ C、generation 据え置き）', async () => {
		const s = setup(noneAnswer);
		const generation = await adopted(s);
		const result = await s.run('recheck');
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(result.snapshot.generation).toBe(generation);
	});

	it('ロックダウンを確認したら end → 通常の確認（トークンが無ければ none = login）', async () => {
		const s = setup(noneAnswer);
		await adopted(s);
		s.setStatus(LOCKED);
		const result = await s.run('recheck');
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.status).toBe('none');
	});
});

describe('runCommissioningPolicy: 期限（全体で共有する絶対時刻）', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});

	it('状態の取得が返らなければ、期限で unverified（PolicyTimeoutError）。signal は止まっている', async () => {
		const s = setup(noneAnswer);
		s.setStatus(
			() =>
				new Promise<CommissioningStatus | null>((resolve) => {
					// 本物の fetchCommissioningStatusOrNull と同じく、中断されたら null。
					s.signals.at(-1)?.addEventListener('abort', () => resolve(null));
				})
		);
		const pending = s.run('guard');
		await vi.advanceTimersByTimeAsync(COMMISSIONING_POLICY_DEADLINE_MS);
		const result = await pending;
		expect(result.outcome).toBe('unverified');
		if (result.outcome === 'unverified') expect(result.error).toBeInstanceOf(PolicyTimeoutError);
		expect(s.signals[0].aborted).toBe(true);
		expect(s.provider.resolve).not.toHaveBeenCalled(); // resolveSettled に落とさない（S-70）
	});

	it('取得関数が中断に応じなくても、期限で unverified（待ち続けない）', async () => {
		const s = setup(noneAnswer);
		s.setStatus(() => new Promise<CommissioningStatus | null>(() => {}));
		const pending = s.run('recheck');
		await vi.advanceTimersByTimeAsync(COMMISSIONING_POLICY_DEADLINE_MS);
		const result = await pending;
		expect(result.outcome).toBe('unverified');
		if (result.outcome === 'unverified') expect(result.error).toBeInstanceOf(PolicyTimeoutError);
	});

	it('期限は状態の取得と通常の確認の全体で共有する（6 秒 + 返らない確認 → 合計 10 秒で unverified）', async () => {
		const s = setup(() => new Promise(() => {})); // provider の確認は返らない
		s.setStatus(
			() =>
				new Promise<CommissioningStatus | null>((resolve) => {
					setTimeout(() => resolve(LOCKED), 6_000);
				})
		);
		let settled = false;
		const pending = s.run('guard').then((r) => {
			settled = true;
			return r;
		});
		await vi.advanceTimersByTimeAsync(9_999);
		expect(settled).toBe(false);
		await vi.advanceTimersByTimeAsync(1);
		const result = await pending;
		expect(result.outcome).toBe('unverified');
		if (result.outcome === 'unverified') expect(result.error).toBeInstanceOf(PolicyTimeoutError);
	});
});

describe('本物の HTTP ヘルパーを通した中断（チェックリスト §5、PR #447 の再レビューと同型）', () => {
	interface SentRequest {
		path: string;
		signal: AbortSignal | null | undefined;
		settled: boolean;
	}

	it('/api/commissioning/status が応答しないまま期限を過ぎると、その要求は止まる。繰り返しても積み重ならない', async () => {
		vi.useFakeTimers();
		const provider = fakeAuthProvider(noneAnswer);
		// commissioning.ts の httpRequest が getAuthProvider() を読むので、既定の登録をしておく。
		initBanto({ dataProvider: {} as DataProvider, authProvider: provider.auth, resources: [] });
		const controller: SessionController = createSessionController(provider.auth);
		const sent: SentRequest[] = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
				const request: SentRequest = { path: String(url), signal: init?.signal, settled: false };
				sent.push(request);
				return new Promise<Response>((_resolve, reject) => {
					request.signal?.addEventListener('abort', () => {
						request.settled = true;
						reject(new DOMException('The operation was aborted.', 'AbortError'));
					});
				});
			})
		);

		for (let i = 0; i < 3; i++) {
			const pending = runCommissioningPolicy(controller, { mode: 'recheck' }); // 既定 = 本物の取得
			await vi.advanceTimersByTimeAsync(COMMISSIONING_POLICY_DEADLINE_MS);
			const result = await pending;
			expect(result.outcome).toBe('unverified');
		}
		expect(sent.map((r) => r.path)).toEqual(Array(3).fill('/api/commissioning/status'));
		expect(sent.every((r) => r.signal?.aborted === true)).toBe(true);
		expect(sent.filter((r) => !r.settled)).toHaveLength(0);
	});
});

describe('resolveSettled との関係（参考: adopt 中の確定）', () => {
	it('S-45: adopt 中は resolveSettled も provider に問い合わせず C を返す（だからロックダウンは end が要る）', async () => {
		const s = setup(noneAnswer);
		await adopted(s);
		s.controller.signal('unauthorized');
		const result = await resolveSettled(s.controller, { cause: 'signal' });
		expect(result.outcome).toBe('confirmed');
		expect(result.snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(s.provider.resolve).not.toHaveBeenCalled();
	});
});
