/**
 * 試運転モードの policy runner（banto v2.0.0 #260、`docs/session-controller-design.md`
 * §6.2「試運転の policy runner」、S-44・S-45・S-53・S-62・S-70・S-71）。
 *
 * 試運転モード（未ロックダウン）中、サーバーは認証の有無に関わらず全リクエストを
 * 合成 admin として受け付けるが、`/api/auth/identity` はその合成 identity を返さない
 * （`commissioning.ts` の `COMMISSIONING_IDENTITY`）。だから provider の確認では
 * 確定できず、**このアプリの方針**として SessionController に
 * `adopt(COMMISSIONING_IDENTITY, 'commissioning', ticket)` で確定させ、ロックダウンが
 * 確定したら `end('commissioning-locked', ticket)` で終わらせる。`adopt`/`end` を
 * 呼ぶのはこのファイルだけ（設計 §6.2 の表「`adopt()` を使うのはこれだけ」）。
 * banto（admin-core）には入れない（アプリ層、設計 §6.2）。
 *
 * ## 2 つの mode
 *
 * | mode | 呼び出し元 | 試運転の状態が取れなかった（`null`） |
 * | --- | --- | --- |
 * | `'guard'` | `(app)/+layout.ts`（毎回の load） | **迂回しない**側に倒す（v1 の `+layout.ts` と同じ安全側）。試運転の合成セッションが確定していれば `end` してから通常の確認 |
 * | `'recheck'` | `sessionRecheck.ts`（再接続の失敗後、試運転中だけ） | `unverified`（`end` も `adopt` もしない、画面を保つ。v1 の `sessionRecheck.ts` と同じ） |
 *
 * 取得の失敗（`null`）と「ロックダウンが確定した」（`lockedDown: true`）は区別する
 * （S-71）。各回の判断は純関数 {@link decideCommissioningStep}（表で総当たりを
 * テストする、`commissioningPolicy.test.ts`）。
 *
 * ## 手順（1 回分）
 *
 * 1. `controller.ticket()` を取る（adopt 中なら epoch だけ、I-21）。
 * 2. `fetchCommissioningStatusOrNull(abort.signal)` を待つ。**この runner の
 *    `AbortSignal` を必ず渡す**（実装チェックリスト §5「時間切れで見捨てた
 *    非同期処理の副作用」: 期限で見捨てた後に、応答しない要求を残さない）。
 *    controller の probe にはこの signal を渡さない（I-22、共有の probe）。
 * 3. 判断:
 *    - `adopt`: `adopt(C, 'commissioning', t)`。`false`（ticket が失効、S-53）なら
 *      新しい ticket でやり直す。
 *    - `end`: `end('commissioning-locked', t)`。`false` ならやり直す（S-62）。
 *    - `handOff`: 通常の確認（`resolveSettled`）へ残りの期限で引き継ぐ。
 *    - `statusUnavailable`: `unverified`（recheck だけ）。
 * 4. `adopt`/`end` が成功したら通常の確認へ引き継ぐ（adopt 中なら controller は
 *    C を `confirmed` で即返す）。
 *
 * ## 期限と上限
 *
 * 期限（`deadlineMs`、既定 10 秒）は開始時に**絶対時刻**で固定し、試運転の状態の
 * 取得と通常の確認の**全体**で共有する（v1 の `sessionRecheck.ts` と同じ）。
 * 期限切れ・やり直しの上限（`maxRounds`、既定 3）・取得の失敗（recheck）は、どれも
 * `resolveSettled` に落とさず `unverified`（既存の snapshot はそのまま。C を
 * `confirmed` にしない、S-70）。`guard` では load が 503 の再試行画面、`recheck` では
 * `'unverified'`（画面を保ち、モニタは再接続を続ける）になる。
 */
import {
	resolveSettled,
	type ResolveResult,
	type SessionController,
	type SessionSnapshot
} from '@banto/admin-core';
import {
	COMMISSIONING_IDENTITY,
	fetchCommissioningStatusOrNull,
	shouldBypassLoginForCommissioning,
	type CommissioningStatus
} from './commissioning';

/** controller に `adopt` で確定させる試運転の合成セッションの kind（owner は `commissioning:commissioning`）。 */
export const COMMISSIONING_KIND = 'commissioning';

/** `end()` に渡す理由。 */
export const COMMISSIONING_LOCKED_REASON = 'commissioning-locked';

/** policy runner の全体の期限の既定（`resolveSettled` の既定と同じ 10 秒）。 */
export const COMMISSIONING_POLICY_DEADLINE_MS = 10_000;

/** `adopt`/`end` が `false` だったときのやり直しを含めた回数の上限の既定。 */
export const COMMISSIONING_POLICY_MAX_ROUNDS = 3;

/** `resolveSettled` と同じく、`superseded` を含まない結果。 */
export type SettledResult = Exclude<ResolveResult, { outcome: 'superseded' }>;

export type CommissioningPolicyMode = 'guard' | 'recheck';

/** 期限（全体で共有）を過ぎた。 */
export class PolicyTimeoutError extends Error {
	constructor() {
		super('試運転モードの確認が期限内に終わりませんでした。');
		this.name = 'PolicyTimeoutError';
	}
}

/** `adopt`/`end` のやり直しが上限に達した（セッションが動き続けた）。 */
export class PolicyExhaustedError extends Error {
	constructor() {
		super('試運転モードの確認の間にセッションが変わり続けました。');
		this.name = 'PolicyExhaustedError';
	}
}

/** 試運転モードの状態を取得できなかった（recheck だけ。guard は迂回しない側に倒す）。 */
export class PolicyStatusUnavailableError extends Error {
	constructor() {
		super('試運転モードの状態を取得できませんでした。');
		this.name = 'PolicyStatusUnavailableError';
	}
}

/**
 * 1 回分の判断（純関数）。
 *
 * - `adopt`: 試運転モードが確定（`lockedDown: false`）。合成セッションを確定する
 *   （すでに確定していれば同じ C の再 adopt で generation は据え置き、S-44）。
 * - `end`: ロックダウンが確定（`lockedDown: true`）、または guard で取得に失敗、
 *   かつ試運転の合成セッションが確定している。終わらせてから通常の確認へ。
 * - `handOff`: 同上で、試運転の合成セッションは確定していない。通常の確認へ。
 * - `statusUnavailable`: recheck で取得に失敗。何も変えずに `unverified`。
 */
export type CommissioningStep = 'adopt' | 'end' | 'handOff' | 'statusUnavailable';

export function decideCommissioningStep(
	mode: CommissioningPolicyMode,
	status: CommissioningStatus | null,
	commissioningLive: boolean
): CommissioningStep {
	if (status === null && mode === 'recheck') return 'statusUnavailable';
	if (shouldBypassLoginForCommissioning(status)) return 'adopt';
	return commissioningLive ? 'end' : 'handOff';
}

/** 試運転の合成セッションが確定しているか（`sessionStore.commissioningMode` と同じ条件）。 */
export function isCommissioningSession(snapshot: SessionSnapshot): boolean {
	return snapshot.status === 'active' && snapshot.kind === COMMISSIONING_KIND;
}

export interface CommissioningPolicyOptions {
	mode: CommissioningPolicyMode;
	deadlineMs?: number;
	maxRounds?: number;
}

/** テストが差し替える口。既定は本物の HTTP ヘルパーと実時間。 */
export interface CommissioningPolicyDeps {
	/** 試運転の状態。失敗（中断を含む）は `null`。既定は `fetchCommissioningStatusOrNull`。 */
	fetchStatus?: (signal: AbortSignal) => Promise<CommissioningStatus | null>;
	/** 現在時刻（ミリ秒）。既定は `Date.now`。 */
	clock?: () => number;
}

export async function runCommissioningPolicy(
	controller: SessionController,
	options: CommissioningPolicyOptions,
	deps: CommissioningPolicyDeps = {}
): Promise<SettledResult> {
	const { mode } = options;
	const deadlineMs = options.deadlineMs ?? COMMISSIONING_POLICY_DEADLINE_MS;
	const maxRounds = options.maxRounds ?? COMMISSIONING_POLICY_MAX_ROUNDS;
	const fetchStatus = deps.fetchStatus ?? fetchCommissioningStatusOrNull;
	const clock = deps.clock ?? (() => Date.now());

	// 方針自身の signal。方針の通信（試運転の状態）はすべてこれで止める。
	// controller の probe には渡さない（共有の probe、I-22）。
	const abort = new AbortController();
	const timer = setTimeout(() => abort.abort(), deadlineMs);
	const deadlineAt = clock() + deadlineMs;
	const remaining = (): number => deadlineAt - clock();
	// 期限は取得関数が signal に従うかどうかに依らず守る（中断に応じない取得でも
	// 待ち続けない）。見捨てる取得は読むだけ（副作用なし）なので、遅れて返っても害は無い。
	const deadlineReached = new Promise<null>((resolve) => {
		abort.signal.addEventListener('abort', () => resolve(null), { once: true });
	});

	// 既存の snapshot は保つ。confirmed にはしない（S-70）。
	const incomplete = (error: Error): SettledResult => ({
		outcome: 'unverified',
		error,
		snapshot: controller.snapshot
	});
	const handOff = async (): Promise<SettledResult> => {
		if (remaining() <= 0) return incomplete(new PolicyTimeoutError());
		const result = await resolveSettled(controller, {
			cause: mode === 'recheck' ? 'signal' : 'navigation',
			deadlineMs: remaining()
		});
		return result.outcome === 'unverified' && remaining() <= 0
			? incomplete(new PolicyTimeoutError())
			: result;
	};

	try {
		for (let round = 0; round < maxRounds; round++) {
			const ticket = controller.ticket();
			const status = await Promise.race([fetchStatus(abort.signal), deadlineReached]);
			if (abort.signal.aborted || remaining() <= 0) return incomplete(new PolicyTimeoutError());
			const step = decideCommissioningStep(
				mode,
				status,
				isCommissioningSession(controller.snapshot)
			);
			switch (step) {
				case 'statusUnavailable':
					return incomplete(new PolicyStatusUnavailableError());
				case 'adopt':
					if (controller.adopt(COMMISSIONING_IDENTITY, COMMISSIONING_KIND, ticket)) {
						return await handOff();
					}
					continue; // ticket が失効（S-53）: 状態の確認からやり直す
				case 'end':
					if (controller.end(COMMISSIONING_LOCKED_REASON, ticket)) return await handOff();
					continue; // 別の判定が先に adopt/end した（S-62）: やり直す
				case 'handOff':
					return await handOff();
			}
		}
		return incomplete(new PolicyExhaustedError());
	} finally {
		// `return await` なので、引き継いだ確認が終わるまでここは走らない。
		clearTimeout(timer);
		abort.abort(); // 念のため: 方針の要求を後に残さない
	}
}
