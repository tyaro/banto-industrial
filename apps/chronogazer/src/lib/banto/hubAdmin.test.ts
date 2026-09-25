/**
 * `hubAdmin.ts` の純関数（6 状態 → 画面文言のマッピング）に対するユニット
 * テスト（#332 chronogazer 分）。`categories.test.ts` と同じ
 * describe/it スタイルで、依存ゼロで直接 import できる範囲だけを固定する
 * （`invoke`/`fetch` を伴う関数は E2E と Rust 側のテストが担当する）。
 *
 * ここで固定したい核心は受入条件の 2 点:
 * 1. **6 状態がすべて別の文言になる**（どれか 2 つが同じ表示に潰れない）。
 * 2. **`connected` の `tagCount: 0` は失敗ではない** - 「接続済み・利用
 *    可能なタグなし」という専用の文言になる。
 *
 * 後半（#383 段階1）は購読状態（7 状態）→ 文言のマッピング。こちらの核心は
 * 「`stopped` は理由を必ず併記する」「`live`/`reconnecting`/`unauthorized`
 * のような別々の事実を同じ表示に潰さない」の 2 点。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	adoptionResult,
	applyServerSelection,
	hubAbandonedDisplay,
	effectiveCredentialGuidance,
	hubCredentialGuidance,
	hubCredentialGuidanceLine,
	hubStatusDisplay,
	isHubStatusStale,
	HUB_STATUS_RECHECKING_LABEL,
	subscriptionCredentialSignal,
	hubPollStaleNote,
	hubStatusDetail,
	hubStatusLabel,
	hubSubscriptionDetail,
	hubSubscriptionHeadline,
	hubSubscriptionLabel,
	hubLastValueLabel,
	hubTimeLabel,
	hubRemainderNote,
	hubUnreachableCauseLabel,
	HUB_UI_REREAD_TIMEOUT_MS,
	HUB_UI_TIMEOUT_MS,
	isPollGenerationCurrent,
	isPollResultFresh,
	isSubscriptionStale,
	nextPollFailureCount,
	nextSelectionUnsaved,
	nextStatusUnconfirmed,
	pollFailureOutcome,
	readSubscriptionWithLimit,
	reconfirmStatusFailureNotice,
	runWithLimit,
	sameSelection,
	saveSelectionWithLimits,
	SELECTION_DISCARDED_NOTICE,
	showsServerSelectionDiff,
	needsManualKey,
	showManualKeyEntry,
	SUBSCRIPTION_POLL_FAILURE_LIMIT,
	SUBSCRIPTION_POLL_TIMEOUT_MS,
	type HubStatus,
	type HubSubscription,
	type HubSubscriptionState,
	type HubView,
	type SelectionEvent,
	type StatusRereadDetailedOutcome,
	type StatusRereadOutcome
} from './hubAdmin';

const ALL_STATES: HubStatus[] = [
	{ state: 'notConfigured' },
	{ state: 'connected', tagCount: 3 },
	{ state: 'authFailed' },
	{ state: 'forbidden' },
	// #446: トリップ（キーは捨てない）。
	{ state: 'keyTripped' },
	{ state: 'unreachable', cause: 'transport' },
	{ state: 'needsPairing' }
];

describe('hubStatusLabel', () => {
	it('6状態がそれぞれ別の見出しになる（どれかが同じ表示に潰れない）', () => {
		const labels = ALL_STATES.map(hubStatusLabel);
		expect(new Set(labels).size).toBe(ALL_STATES.length);
		expect(labels).not.toContain('');
	});

	it('タグ0件は失敗ではなく「接続済み・利用可能なタグなし」になる', () => {
		expect(hubStatusLabel({ state: 'connected', tagCount: 0 })).toBe(
			'接続済み・利用可能なタグなし'
		);
		expect(hubStatusLabel({ state: 'connected', tagCount: 2 })).toBe('接続済み（タグ2件）');
	});

	it('到達不能・認証失敗・権限不足・連携要求は互いに区別される', () => {
		expect(hubStatusLabel({ state: 'unreachable', cause: 'transport' })).toBe(
			'Hubに到達できません'
		);
		expect(hubStatusLabel({ state: 'authFailed' })).toBe('認証に失敗');
		expect(hubStatusLabel({ state: 'forbidden' })).toBe('権限が不足');
		expect(hubStatusLabel({ state: 'needsPairing' })).toBe('連携が必要');
	});
});

describe('hubStatusDetail', () => {
	it('どの状態でも「次に何をすればよいか」の説明が付く', () => {
		for (const status of ALL_STATES) {
			expect(hubStatusDetail(status).length).toBeGreaterThan(0);
		}
	});

	it('タグ0件の説明は「接続できているがタグが無い」ことを述べ、失敗として扱わない', () => {
		const detail = hubStatusDetail({ state: 'connected', tagCount: 0 });
		expect(detail).toContain('タグが登録されていません');
	});

	it('到達不能の説明には原因の日本語が含まれる', () => {
		expect(hubStatusDetail({ state: 'unreachable', cause: 'server_error' })).toContain(
			hubUnreachableCauseLabel('server_error')
		);
	});

	it('連携が必要の説明はロックダウン済みで自動発行しないことを述べる', () => {
		expect(hubStatusDetail({ state: 'needsPairing' })).toContain('ロックダウン');
	});
});

describe('hubUnreachableCauseLabel', () => {
	it('原因ごとに別の文言になる', () => {
		const labels = (['transport', 'protocol', 'server_error', 'invalid_endpoint'] as const).map(
			hubUnreachableCauseLabel
		);
		expect(new Set(labels).size).toBe(4);
	});
});

describe('needsManualKey', () => {
	it('手入力欄はロックダウン済み（連携が必要）と権限不足のときだけ出す', () => {
		expect(needsManualKey({ state: 'needsPairing' })).toBe(true);
		expect(needsManualKey({ state: 'forbidden' })).toBe(true);
	});

	it('再発行で直る状態・正常な状態では手入力欄を出さない', () => {
		// authFailed は「接続」で再発行できるので、まず手入力を促さない。
		expect(needsManualKey({ state: 'authFailed' })).toBe(false);
		expect(needsManualKey({ state: 'notConfigured' })).toBe(false);
		expect(needsManualKey({ state: 'connected', tagCount: 0 })).toBe(false);
		expect(needsManualKey({ state: 'unreachable', cause: 'transport' })).toBe(false);
	});
});

// --- #383 段階1: 購読状態 → 画面文言 ----------------------------------------

const ALL_SUBSCRIPTION_STATES: HubSubscriptionState[] = [
	'stopped',
	'connecting',
	'handshaking',
	'live',
	'rebinding',
	'reconnecting',
	'unauthorized'
];

function subscription(overrides: Partial<HubSubscription> = {}): HubSubscription {
	return {
		state: 'live',
		reason: null,
		subscribedCount: 2,
		unresolved: [],
		unsupported: [],
		lastError: null,
		lastValueAt: 1000,
		values: [],
		...overrides
	};
}

describe('hubSubscriptionLabel', () => {
	it('7状態すべてに空でない文言が付く', () => {
		for (const state of ALL_SUBSCRIPTION_STATES) {
			expect(hubSubscriptionLabel(state).length).toBeGreaterThan(0);
		}
	});

	it('connecting と handshaking だけが意図的に同じ文言で、他は互いに潰れない', () => {
		expect(hubSubscriptionLabel('connecting')).toBe(hubSubscriptionLabel('handshaking'));
		const distinct = new Set(ALL_SUBSCRIPTION_STATES.map(hubSubscriptionLabel));
		expect(distinct.size).toBe(ALL_SUBSCRIPTION_STATES.length - 1);
	});

	it('受信中・再接続中・認証エラー・停止は別々の事実として区別される', () => {
		expect(hubSubscriptionLabel('live')).toBe('受信中');
		expect(hubSubscriptionLabel('reconnecting')).toBe('再接続中');
		expect(hubSubscriptionLabel('rebinding')).toBe('再バインド中');
		expect(hubSubscriptionLabel('unauthorized')).toBe('認証エラー');
		expect(hubSubscriptionLabel('stopped')).toBe('停止');
	});
});

describe('hubSubscriptionDetail', () => {
	it('stopped のときは Rust 側の理由をそのまま併記する', () => {
		expect(
			hubSubscriptionDetail(
				subscription({ state: 'stopped', reason: '購読するタグが選ばれていません。' })
			)
		).toBe('購読するタグが選ばれていません。');
	});

	it('理由が無い stopped でも説明を空欄にしない', () => {
		const detail = hubSubscriptionDetail(subscription({ state: 'stopped', reason: null }));
		expect(detail.length).toBeGreaterThan(0);
	});

	it('購読中は件数を述べ、停止の理由文言とは別物になる', () => {
		const live = hubSubscriptionDetail(subscription({ state: 'live', subscribedCount: 3 }));
		expect(live).toContain('3');
		expect(live).toContain('購読しています');
		expect(live).not.toBe(hubSubscriptionDetail(subscription({ state: 'stopped', reason: 'x' })));
	});

	it('進行中の状態では「購読しています」と言い切らない（まだ受信していない）', () => {
		// これらの状態では `current()` が値を返さない＝実際には受信していない。
		// 「N件のタグを購読しています」と出すと状態ラベルと矛盾する。
		for (const state of ['connecting', 'handshaking', 'rebinding', 'reconnecting'] as const) {
			const detail = hubSubscriptionDetail(subscription({ state, subscribedCount: 3 }));
			expect(detail, state).not.toContain('購読しています');
			expect(detail.length, state).toBeGreaterThan(0);
		}
	});

	it('進行中の状態の説明は互いに潰れない', () => {
		const details = (
			['live', 'connecting', 'handshaking', 'rebinding', 'reconnecting'] as const
		).map((state) => hubSubscriptionDetail(subscription({ state })));
		// connecting と handshaking は見出しと同じく意図的に同じ説明。
		expect(details[1]).toBe(details[2]);
		expect(new Set(details).size).toBe(details.length - 1);
	});

	it('unauthorized は接続側と同じ導線（再発行 / キーの採用）へ誘導する', () => {
		const detail = hubSubscriptionDetail(subscription({ state: 'unauthorized' }));
		expect(detail).toContain('接続');
		expect(detail).toContain('採用');
		// 「接続設定の状態とは別」と言い切らない - ユーザーにとっては同じ
		// 「認証が通っていない」であり、別軸なのは内部の話。
		expect(detail).toContain('認証が通っていません');
	});

	it('未解決タグは状態に関わらず保持され、空表示に潰れない', () => {
		// 文言側は unresolved を消さない - 表示の責務は画面だが、型として
		// 残っていることをここで固定しておく（「タグ0件」に潰さない）。
		const stopped = subscription({ state: 'stopped', reason: 'r', unresolved: ['a', 'b'] });
		expect(stopped.unresolved).toEqual(['a', 'b']);
		expect(hubSubscriptionDetail(stopped)).toBe('r');
	});

	it('stopped + lastError は、件数が残っていても「購読しています」にしない', () => {
		// ワーカーが終端エラーで止まると世代は残る（subscribedCount > 0）まま
		// state だけ stopped になる。ここで件数を根拠に「購読中」と言うと
		// 状態表示（停止）と説明が矛盾する。
		const detail = hubSubscriptionDetail(
			subscription({
				state: 'stopped',
				reason: null,
				lastError: 'unauthorized',
				subscribedCount: 3
			})
		);
		expect(detail).not.toContain('購読しています');
		expect(detail).toContain('停止');
		expect(detail).toContain('unauthorized');
	});

	it('stopped で reason も lastError も無ければ、単に購読していないと述べる', () => {
		const detail = hubSubscriptionDetail(
			subscription({ state: 'stopped', reason: null, lastError: null, subscribedCount: 0 })
		);
		expect(detail).toBe('購読していません。');
	});

	it('購読できない名前は未解決タグと別のバケツで保持される（理由が違うものを混ぜない）', () => {
		const stopped = subscription({
			state: 'stopped',
			reason: 'r',
			unresolved: ['gone'],
			unsupported: ['a,b']
		});
		expect(stopped.unresolved).toEqual(['gone']);
		expect(stopped.unsupported).toEqual(['a,b']);
	});
});

describe('showManualKeyEntry', () => {
	it('接続側が連携要求・権限不足のときは従来どおり出す', () => {
		expect(showManualKeyEntry({ state: 'needsPairing' }, null)).toBe(true);
		expect(showManualKeyEntry({ state: 'forbidden' }, null)).toBe(true);
	});

	it('接続は connected でも購読だけ unauthorized なら出す（直す手段を残す）', () => {
		// catalog は読めていて WS のハンドシェイクだけが 401/403 の場合。
		// 接続側の状態だけを見ていると手動キーの導線に到達できない。
		expect(
			showManualKeyEntry(
				{ state: 'connected', tagCount: 3 },
				subscription({ state: 'unauthorized' })
			)
		).toBe(true);
	});

	it('購読が正常なら接続側の状態にだけ従う', () => {
		expect(
			showManualKeyEntry({ state: 'connected', tagCount: 3 }, subscription({ state: 'live' }))
		).toBe(false);
		expect(showManualKeyEntry({ state: 'authFailed' }, subscription({ state: 'stopped' }))).toBe(
			false
		);
		expect(showManualKeyEntry({ state: 'notConfigured' }, null)).toBe(false);
	});
});

/**
 * #446: Hub が close 1008 で購読を打ち切った理由（`lastError`）ごとの案内。
 * トリップは「管理者に解除を依頼」（同じキーで戻る）、失効・期限切れ・存在
 * しないは「新しい API キーを設定」（同じキーでは戻らない）。
 */
describe('hubCredentialGuidance', () => {
	// [lastError, 次の一手, 案内に含む言葉, 含まない言葉]
	const TABLE = [
		[
			'key_tripped',
			'askAdmin',
			['管理者に解除を依頼', '同じキーのまま', '自動で再開'],
			['新しいAPIキー']
		],
		[
			'key_revoked',
			'replaceKey',
			['失効', '新しいAPIキーを設定', '採用'],
			['解除を依頼', '自動で再開します']
		],
		[
			'key_expired',
			'replaceKey',
			['有効期限', '新しいAPIキーを設定', '採用'],
			['解除を依頼', '自動で再開します']
		],
		[
			'key_not_found',
			'replaceKey',
			['見つからない', '新しいAPIキーを設定', '採用'],
			['解除を依頼', '自動で再開します']
		],
		[
			'credential_rejected',
			'checkHub',
			['理由を判別できません', '管理者に確認'],
			['新しいAPIキーを設定']
		]
	] as const;

	it('理由ごとに次の一手と案内が決まる', () => {
		for (const [lastError, action, includes, excludes] of TABLE) {
			const guidance = hubCredentialGuidance(lastError);
			expect(guidance, lastError).not.toBeNull();
			expect(guidance?.reason, lastError).toBe(lastError);
			expect(guidance?.action, lastError).toBe(action);
			for (const word of includes) expect(guidance?.message, lastError).toContain(word);
			for (const word of excludes) expect(guidance?.message, lastError).not.toContain(word);
		}
	});

	it('理由ごとの案内は互いに潰れない', () => {
		const messages = TABLE.map(([lastError]) => hubCredentialGuidance(lastError)?.message);
		expect(new Set(messages).size).toBe(TABLE.length);
	});

	it('1008 の分類でなければ案内しない（従来の文言に任せる）', () => {
		for (const lastError of [
			null,
			'unauthorized',
			'transport',
			'binding_unresolved',
			'future_kind'
		]) {
			expect(hubCredentialGuidance(lastError), String(lastError)).toBeNull();
		}
	});

	it('unauthorized の説明文が理由ごとの案内になる', () => {
		for (const [lastError] of TABLE) {
			expect(hubSubscriptionDetail(subscription({ state: 'unauthorized', lastError }))).toBe(
				hubCredentialGuidance(lastError)?.message
			);
		}
		// 理由の無い 401/403 は従来の文言のまま。
		expect(
			hubSubscriptionDetail(subscription({ state: 'unauthorized', lastError: 'unauthorized' }))
		).toContain('認証が通っていません');
	});

	it('世代を止めた後（stopped + 理由）は、停止の理由と別の行で案内する', () => {
		const stopped = subscription({
			state: 'stopped',
			reason: '保存済みのAPIキーがHubに拒否されたため購読できません。',
			lastError: 'key_tripped'
		});
		expect(hubSubscriptionDetail(stopped)).toBe(stopped.reason);
		expect(hubCredentialGuidanceLine(stopped)).toBe(hubCredentialGuidance('key_tripped')?.message);
		// unauthorized では説明文が案内そのものなので、別の行は出さない。
		expect(
			hubCredentialGuidanceLine(subscription({ state: 'unauthorized', lastError: 'key_tripped' }))
		).toBeNull();
		expect(
			hubCredentialGuidanceLine(subscription({ state: 'stopped', lastError: null }))
		).toBeNull();
		expect(hubCredentialGuidanceLine(null)).toBeNull();
	});

	it('失効・期限切れ・存在しないは、止めた後もキーの入力欄を出す（トリップは出さない）', () => {
		for (const [lastError, action] of TABLE) {
			expect(
				showManualKeyEntry({ state: 'authFailed' }, subscription({ state: 'stopped', lastError })),
				lastError
			).toBe(action === 'replaceKey');
		}
	});
});

/**
 * #446（監査 P2）: 接続の状態 × 購読の状態 × `lastError` の全組み合わせで、
 * 画面が「キーを捨てるな」（トリップ）と「キーを替えよ」を同時に言わない。
 *
 * 画面に出るもの = 接続の状態の説明（`hubStatusDetail`）・購読の説明
 * （`hubSubscriptionDetail`）・案内の行（`hubCredentialGuidanceLine`）・
 * 手動キーの入力欄（`showManualKeyEntry`）。
 */
describe('接続の状態と購読の案内の組み合わせ', () => {
	const LAST_ERRORS = [
		null,
		'unauthorized',
		'transport',
		'key_tripped',
		'key_revoked',
		'key_expired',
		'key_not_found',
		'credential_rejected'
	];
	const SUBSCRIPTION_STATES = ['stopped', 'unauthorized', 'live', 'reconnecting'] as const;
	// 「キーを捨てるな」（トリップの案内は必ずこれを含む。失効などの案内の
	// 「同じキーのままでは自動で再開しません」は逆の意味なので数えない）
	const KEEP_KEY = ['解除を依頼'];
	// 「キーを替えよ」
	const REPLACE_KEY = ['採用', '再発行', '新しいAPIキー', '貼り付け'];

	function screen(status: HubStatus, sub: HubSubscription) {
		const texts = [
			hubStatusDetail(status),
			hubSubscriptionDetail(sub, status),
			hubCredentialGuidanceLine(sub, status) ?? ''
		];
		return { texts, field: showManualKeyEntry(status, sub) };
	}

	it('トリップを保てと言いながら、キーの入れ替えを勧めない（入力欄も出さない）', () => {
		for (const status of ALL_STATES) {
			for (const state of SUBSCRIPTION_STATES) {
				for (const lastError of LAST_ERRORS) {
					for (const reason of [null, '購読を止めています。']) {
						const sub = subscription({ state, lastError, reason });
						const { texts, field } = screen(status, sub);
						const label = `${status.state} × ${state} × ${lastError} × reason=${reason}`;
						const keep = texts.some((text) => KEEP_KEY.some((word) => text.includes(word)));
						const replace =
							field || texts.some((text) => REPLACE_KEY.some((word) => text.includes(word)));
						expect(keep && replace, label).toBe(false);
					}
				}
			}
		}
	});

	it('トリップ中（keyTripped）は、購読の理由に関わらずトリップの案内に揃い、入力欄を出さない', () => {
		for (const state of SUBSCRIPTION_STATES) {
			for (const lastError of LAST_ERRORS) {
				const sub = subscription({ state, lastError });
				expect(showManualKeyEntry({ state: 'keyTripped' }, sub), `${state} × ${lastError}`).toBe(
					false
				);
				expect(effectiveCredentialGuidance({ state: 'keyTripped' }, lastError)?.action).toBe(
					'askAdmin'
				);
			}
		}
		expect(hubStatusLabel({ state: 'keyTripped' })).toBe('キーがトリップ中');
		expect(hubStatusDetail({ state: 'keyTripped' })).toContain('解除を依頼');
		expect(hubStatusDetail({ state: 'keyTripped' })).toContain('同じキーのまま');
		// 案内の行は接続の状態の説明と重複するので出さない。
		expect(
			hubCredentialGuidanceLine(
				subscription({ state: 'stopped', reason: 'r', lastError: 'key_tripped' }),
				{ state: 'keyTripped' }
			)
		).toBeNull();
	});

	it('REST が「キーが無効／権限が無い」と言っているときは、古いトリップの案内を出さない', () => {
		for (const status of [
			{ state: 'authFailed' },
			{ state: 'forbidden' },
			{ state: 'needsPairing' }
		] as HubStatus[]) {
			expect(effectiveCredentialGuidance(status, 'key_tripped'), status.state).toBeNull();
			// 失効などの案内はそのまま（どちらも「キーを替えよ」で揃っている）。
			expect(effectiveCredentialGuidance(status, 'key_revoked')?.action).toBe('replaceKey');
		}
		// 接続の状態が別の話（接続済み・到達不能）なら、購読の理由の案内のまま。
		expect(
			effectiveCredentialGuidance({ state: 'connected', tagCount: 1 }, 'key_tripped')?.action
		).toBe('askAdmin');
	});

	it('失効・期限切れ・存在しないは、接続の状態が authFailed でも入力欄と案内が揃う', () => {
		for (const lastError of ['key_revoked', 'key_expired', 'key_not_found']) {
			const sub = subscription({ state: 'stopped', reason: 'r', lastError });
			const { texts, field } = screen({ state: 'authFailed' }, sub);
			expect(field, lastError).toBe(true);
			expect(
				texts.some((text) => text.includes('新しいAPIキー')),
				lastError
			).toBe(true);
		}
	});
});

/**
 * #449 レビュー P2-2: 画面を開き直さない時系列。接続の状態は画面を開いたとき・
 * 明示操作のときにしか取り直さず、購読の状態だけが 2 秒ごとに更新される。
 * 古い接続の状態で、新しい拒否の理由を抑えない。
 */
describe('接続の状態が購読より古くなったとき（時系列）', () => {
	/** 画面が実際に出すもの（HubSection と同じ組み立て）。 */
	function render(
		status: HubStatus,
		observedWith: HubSubscription | null,
		current: HubSubscription
	) {
		const display = hubStatusDisplay(status, observedWith, current);
		const texts = [
			display.label,
			display.detail,
			hubSubscriptionDetail(current, display.guidance),
			hubCredentialGuidanceLine(current, display.guidance) ?? ''
		];
		return {
			display,
			texts,
			field: showManualKeyEntry(display.guidance, current),
			keepKey: texts.some((text) => text.includes('解除を依頼'))
		};
	}

	it('keyTripped で開く → 解除されて live → 開いたまま key_revoked', () => {
		// 1. トリップ中に画面を開く（`status()` の応答に接続の状態と購読が一緒に来る）。
		const status: HubStatus = { state: 'keyTripped' };
		const opened = subscription({ state: 'stopped', reason: 'r', lastError: 'key_tripped' });
		let screen = render(status, opened, opened);
		expect(screen.display.stale).toBe(false);
		expect(screen.keepKey).toBe(true);
		expect(screen.field).toBe(false);

		// 2. 管理者が解除し、見張りが張り直して受信中（ポーリングで購読だけ更新）。
		const live = subscription({ state: 'live', lastError: null });
		screen = render(status, opened, live);
		expect(screen.display.stale, '古いトリップの表示を残さない').toBe(true);
		expect(screen.display.label).toBe(HUB_STATUS_RECHECKING_LABEL);
		expect(screen.keepKey).toBe(false);

		// 3. 画面を開いたまま、そのキーが失効する（close 1008 api_key_revoked）。
		const revoked = subscription({ state: 'unauthorized', lastError: 'key_revoked' });
		screen = render(status, opened, revoked);
		expect(screen.display.stale).toBe(true);
		expect(screen.keepKey, '古い keyTripped で「解除を依頼」と言わない').toBe(false);
		expect(hubSubscriptionDetail(revoked, screen.display.guidance)).toBe(
			hubCredentialGuidance('key_revoked')?.message
		);
		expect(screen.field, '交換用の入力欄を隠さない').toBe(true);

		// 4. 取り直した接続の状態（401 = authFailed）が届けば、古くない状態に戻る。
		screen = render({ state: 'authFailed' }, revoked, revoked);
		expect(screen.display.stale).toBe(false);
		expect(screen.keepKey).toBe(false);
		expect(screen.field).toBe(true);
	});

	it('トリップ中に画面を開き、開いたままトリップが続く間は古くならない', () => {
		const status: HubStatus = { state: 'keyTripped' };
		const opened = subscription({ state: 'stopped', reason: 'r', lastError: 'key_tripped' });
		// ポーリングで同じ内容の購読が届き続ける。
		const same = subscription({ state: 'stopped', reason: 'r', lastError: 'key_tripped' });
		expect(isHubStatusStale(status, opened, same)).toBe(false);
	});

	it('キーについて何も言っていない接続の状態は、購読が変わっても古いと扱わない', () => {
		const opened = subscription({ state: 'connecting', lastError: null });
		const live = subscription({ state: 'live', lastError: null });
		for (const status of [
			{ state: 'connected', tagCount: 1 },
			{ state: 'unreachable', cause: 'transport' },
			{ state: 'notConfigured' }
		] as HubStatus[]) {
			expect(isHubStatusStale(status, opened, live), status.state).toBe(false);
		}
		// キーについて言っている状態は、購読のキーに関わる部分が変われば古い。
		for (const status of [
			{ state: 'keyTripped' },
			{ state: 'authFailed' },
			{ state: 'forbidden' },
			{ state: 'needsPairing' }
		] as HubStatus[]) {
			expect(isHubStatusStale(status, opened, live), status.state).toBe(true);
		}
	});

	it('購読のキーに関わる部分', () => {
		expect(subscriptionCredentialSignal(null)).toBeNull();
		expect(subscriptionCredentialSignal(subscription({ state: 'live' }))).toBe('live');
		expect(
			subscriptionCredentialSignal(
				subscription({ state: 'unauthorized', lastError: 'unauthorized' })
			)
		).toBe('unauthorized');
		expect(
			subscriptionCredentialSignal(subscription({ state: 'stopped', lastError: 'key_expired' }))
		).toBe('key_expired');
		expect(
			subscriptionCredentialSignal(subscription({ state: 'reconnecting', lastError: 'transport' }))
		).toBeNull();
	});
});

describe('adoptionResult（採用しなかった候補キーの判定を、保存中のキーの状態にしない）', () => {
	const view = (status: HubStatus): HubView => ({
		status,
		endpoint: 'http://hub',
		keyName: null,
		selectedTags: [],
		tags: null,
		subscription: subscription({ state: 'stopped', reason: 'r', lastError: 'key_revoked' })
	});

	it('候補が通らなければ接続の状態は前のまま、理由は別に伝える', () => {
		const previous: HubStatus = { state: 'authFailed' };
		for (const candidate of [
			{ state: 'keyTripped' },
			{ state: 'forbidden' },
			{ state: 'authFailed' },
			{ state: 'unreachable', cause: 'transport' }
		] as HubStatus[]) {
			const outcome = adoptionResult(previous, view(candidate));
			expect(outcome.status, candidate.state).toBe(previous);
			expect(outcome.adopted).toBe(false);
			expect(outcome.notice).toContain(hubStatusLabel(candidate));
			expect(outcome.notice).toContain('採用できませんでした');
		}
		// 連携が必要な Hub で候補が通らなくても、「連携が必要」と入力欄は残る。
		const pairing = adoptionResult({ state: 'needsPairing' }, view({ state: 'forbidden' }));
		expect(pairing.status).toEqual({ state: 'needsPairing' });
	});

	it('失効した K1 に対して候補 K2 がトリップしていても、「解除を依頼」と言わない', () => {
		const outcome = adoptionResult({ state: 'authFailed' }, view({ state: 'keyTripped' }));
		const current = view({ state: 'keyTripped' }).subscription;
		const texts = [
			hubStatusDetail(outcome.status),
			hubSubscriptionDetail(current, outcome.status),
			hubCredentialGuidanceLine(current, outcome.status) ?? ''
		];
		expect(texts.some((text) => text.includes('解除を依頼'))).toBe(false);
		expect(showManualKeyEntry(outcome.status, current)).toBe(true);
	});

	it('採用できたら、その接続の状態を出す', () => {
		const outcome = adoptionResult(
			{ state: 'needsPairing' },
			view({ state: 'connected', tagCount: 2 })
		);
		expect(outcome).toEqual({
			status: { state: 'connected', tagCount: 2 },
			adopted: true,
			notice: null
		});
	});
});

describe('hubLastValueLabel / hubTimeLabel', () => {
	it('まだ一度も受信していないことを明示する（空欄や 0 に潰さない）', () => {
		expect(hubLastValueLabel(null, 'stopped')).toBe('まだ受信していません');
		expect(hubLastValueLabel(null, 'live')).toBe('まだ受信していません');
	});

	it('受信済みなら epoch ミリ秒をその端末の書式で出す', () => {
		const epochMs = 1722758400123;
		expect(hubLastValueLabel(epochMs, 'live')).toBe(new Date(epochMs).toLocaleString());
		// epoch ミリ秒をそのまま数字で出さない。
		expect(hubLastValueLabel(epochMs, 'live')).not.toBe(String(epochMs));
	});

	it('live でないときは同じ行から「今は受信していない」と分かる', () => {
		// バックエンドは「同じ購読が止まっているだけ」なら時刻を残すので、
		// 時刻だけを出すと受信し続けているように読めてしまう。
		const epochMs = 1722758400123;
		const at = new Date(epochMs).toLocaleString();
		expect(hubLastValueLabel(epochMs, 'stopped')).toBe(`${at}（購読は停止しています）`);
		for (const state of [
			'connecting',
			'handshaking',
			'rebinding',
			'reconnecting',
			'unauthorized'
		] as const) {
			expect(hubLastValueLabel(epochMs, state), state).toBe(`${at}（現在は受信していません）`);
		}
	});

	it('解釈できない値は握りつぶさずそのまま見せる', () => {
		expect(hubTimeLabel(Number.NaN)).toBe('NaN');
	});
});

describe('isPollResultFresh', () => {
	it('明示操作が割り込んでいなければ適用する', () => {
		expect(isPollResultFresh(3, 3)).toBe(true);
	});

	it('待っている間に明示操作の結果が入っていたら捨てる（状態を巻き戻さない）', () => {
		// 接続の前に飛ばしたポーリングが connect() の応答より後に着く場合。
		expect(isPollResultFresh(3, 4)).toBe(false);
	});

	it('view を返さない明示操作（選択の保存）でも番号を進めれば古い応答を捨てられる', () => {
		// 保存は 204 で view を返さないが、設定も購読も変える明示操作。
		// 番号を進めないと、保存中に飛んでいたポーリング応答が保存後に
		// 受け入れられ、古いタグの値と「受信中」を表示してしまう。
		let applied = 0;
		const sentBeforeSave = applied;

		applied += 1; // saveSelection() の beginExplicitChange()
		expect(isPollResultFresh(sentBeforeSave, applied)).toBe(false);

		applied += 1; // 保存後に取り直した購読状態の反映
		expect(isPollResultFresh(sentBeforeSave, applied)).toBe(false);

		// 反映後に送ったポーリングは当然受け入れる。
		const sentAfterSave = applied;
		expect(isPollResultFresh(sentAfterSave, applied)).toBe(true);
	});
});

describe('isPollGenerationCurrent', () => {
	it('停止を跨いでいなければ適用も予約もしてよい', () => {
		expect(isPollGenerationCurrent(2, 2)).toBe(true);
	});

	it('停止（や停止→再開）を跨いだ応答は自分のものではない', () => {
		// タブを隠した瞬間に飛んでいた要求が再表示後に解決する場合。ここで
		// 次のタイマを張ると、再開後のループと二重に回り続ける。
		expect(isPollGenerationCurrent(2, 3)).toBe(false);
	});
});

describe('hubRemainderNote', () => {
	it('購読できているタグがあれば「残りだけを購読している」と言う', () => {
		expect(hubRemainderNote(2)).toContain('残りのタグだけ');
	});

	it('1件も購読していないのに「購読しています」と言わない', () => {
		// 選んだ全部が未解決／購読不可のとき。
		const note = hubRemainderNote(0);
		expect(note).not.toContain('残りのタグだけを購読しています');
		expect(note).toContain('購読していません');
	});
});

// --- レビュー P2-A: ポーリングの恒久的な失敗を「受信中」のまま出さない ------

describe('nextPollFailureCount / isSubscriptionStale', () => {
	it('連続失敗を 0/1/2/3 と数え、2 回目で「取得できていません」に切り替わる', () => {
		// 1 回では切り替えない（一過性の取りこぼしで表示を揺らさない）。
		let failures = 0;
		expect(isSubscriptionStale(failures)).toBe(false);

		failures = nextPollFailureCount(failures, 'failed'); // 1
		expect(failures).toBe(1);
		expect(isSubscriptionStale(failures)).toBe(false);

		failures = nextPollFailureCount(failures, 'failed'); // 2 = 閾値
		expect(failures).toBe(SUBSCRIPTION_POLL_FAILURE_LIMIT);
		expect(isSubscriptionStale(failures)).toBe(true);

		failures = nextPollFailureCount(failures, 'failed'); // 3
		expect(failures).toBe(3);
		expect(isSubscriptionStale(failures)).toBe(true);
	});

	it('1 回でも成功したら 0 に戻り、通常表示に戻る', () => {
		expect(nextPollFailureCount(3, 'ok')).toBe(0);
		expect(isSubscriptionStale(nextPollFailureCount(3, 'ok'))).toBe(false);
		expect(nextPollFailureCount(0, 'ok')).toBe(0);
	});
});

describe('hubSubscriptionHeadline', () => {
	it('取得できている間は既存の状態名そのまま（状態名を増やさない）', () => {
		for (const state of ALL_SUBSCRIPTION_STATES) {
			expect(hubSubscriptionHeadline(state, false)).toBe(hubSubscriptionLabel(state));
		}
	});

	it('取得できていない間は「受信中」と言い切らない', () => {
		const stale = hubSubscriptionHeadline('live', true);
		expect(stale).not.toBe('受信中');
		expect(stale).toContain('受信中'); // 既存の状態名は残す
		expect(stale).toContain('状態を取得できていません');
	});

	it('どの状態でも同じ添え方になる（live だけの特別扱いにしない）', () => {
		for (const state of ALL_SUBSCRIPTION_STATES) {
			expect(hubSubscriptionHeadline(state, true)).toBe(
				`${hubSubscriptionLabel(state)}（状態を取得できていません）`
			);
		}
	});
});

describe('hubPollStaleNote', () => {
	it('いつ取得できた表示なのかを添える（古い値だと分かるようにする）', () => {
		const epochMs = 1722758400123;
		const note = hubPollStaleNote(epochMs);
		expect(note).toContain('取得できていません');
		expect(note).toContain(hubTimeLabel(epochMs));
		expect(note).toContain('最新ではありません');
	});

	it('一度も取得できていないときも、空欄にせずそう言う', () => {
		const note = hubPollStaleNote(null);
		expect(note).toContain('まだ一度も取得できていません');
		expect(note).not.toContain('Invalid Date');
	});
});

// --- レビュー P2-B: 未保存のタグ選択を黙って捨てない -----------------------

describe('sameSelection', () => {
	it('並び順は問わない（画面のチェックは順序を持たない）', () => {
		expect(sameSelection(['a', 'b'], ['b', 'a'])).toBe(true);
		expect(sameSelection([], [])).toBe(true);
	});

	it('件数や中身が違えば false', () => {
		expect(sameSelection(['a'], ['a', 'b'])).toBe(false);
		expect(sameSelection(['a', 'b'], ['a'])).toBe(false);
		expect(sameSelection(['a'], ['b'])).toBe(false);
	});
});

describe('applyServerSelection', () => {
	const HUB_A = 'http://127.0.0.1:3100';
	const HUB_B = 'http://127.0.0.1:3200';

	it('未保存の変更が無ければサーバーの選択で上書きする（従来どおり）', () => {
		const outcome = applyServerSelection(['a'], ['b', 'c'], false, HUB_A, HUB_A);
		expect(outcome.selected).toEqual(['b', 'c']);
		expect(outcome.unsaved).toBe(false);
		expect(outcome.discardedForEndpointChange).toBe(false);
	});

	it('同じHubなら未保存の変更を上書きしない（「一覧を更新」で黙って消えない）', () => {
		const outcome = applyServerSelection(['a', 'x'], ['a'], true, HUB_A, HUB_A);
		expect(outcome.selected).toEqual(['a', 'x']);
		expect(outcome.unsaved).toBe(true);
		expect(outcome.discardedForEndpointChange).toBe(false);
	});

	// レビュー P2-2: 下書きは接続先ごとのもの。別の Hub に繋ぎ直したら、旧 Hub
	// のタグ名を保存の payload に持ち越さない（画面に出ていない名前が混ざる）。
	it('未保存 × 接続先の同一性の総当たり', () => {
		type Row = [boolean, string | null, string | null, string[], boolean, boolean];
		const table: Row[] = [
			// [未保存, 下書きのendpoint, viewのendpoint, 採用する選択, 未保存フラグ, 破棄したか]
			[true, HUB_A, HUB_A, ['a', 'x'], true, false], // 同じHub: 保つ
			[true, HUB_A, HUB_B, ['a'], false, true], // 別のHub: サーバーを採用して破棄
			[true, HUB_A, null, ['a'], false, true], // 未設定・切断後も「同じHub」ではない
			[true, null, HUB_A, ['a'], false, true], // 下書き側が null（接続先不明）
			[true, null, null, ['a'], false, true],
			[false, HUB_A, HUB_A, ['a'], false, false], // 未保存でなければ常にサーバー
			[false, HUB_A, HUB_B, ['a'], false, false],
			[false, null, HUB_B, ['a'], false, false]
		];
		for (const [unsaved, draftEndpoint, viewEndpoint, selected, nextUnsaved, discarded] of table) {
			const outcome = applyServerSelection(['a', 'x'], ['a'], unsaved, draftEndpoint, viewEndpoint);
			const label = `unsaved=${unsaved} ${draftEndpoint} -> ${viewEndpoint}`;
			expect(outcome.selected, label).toEqual(selected);
			expect(outcome.unsaved, label).toBe(nextUnsaved);
			expect(outcome.discardedForEndpointChange, label).toBe(discarded);
		}
	});

	it('同じHubなら、今その一覧に無いタグの選択も落とさない（catalog と積集合を取らない）', () => {
		// 一時的に見えなくなっただけのタグ（refresh が空振り・権限が一瞬揺れた）
		// の選択まで失わないこと。判定は接続先の同一性だけ。
		const outcome = applyServerSelection(['gone', 'here'], [], true, HUB_A, HUB_A);
		expect(outcome.selected).toEqual(['gone', 'here']);
	});

	it('返すのは常に新しい配列（呼び出し側の配列を共有しない）', () => {
		const current = ['a'];
		const server = ['b'];
		expect(applyServerSelection(current, server, true, HUB_A, HUB_A).selected).not.toBe(current);
		expect(applyServerSelection(current, server, false, HUB_A, HUB_A).selected).not.toBe(server);
	});

	it('破棄の通知は保存成功の文言と混ざらない', () => {
		expect(SELECTION_DISCARDED_NOTICE).toContain('破棄');
		expect(SELECTION_DISCARDED_NOTICE).not.toContain('保存しました');
	});
});

describe('showsServerSelectionDiff', () => {
	it('未保存で中身も違うときだけ、注記と「戻す」導線を出す', () => {
		const table: [string[], string[], boolean, boolean][] = [
			// [編集中, サーバー, 未保存か, 注記を出すか]
			[['a'], ['a'], false, false],
			[['a'], ['b'], false, false], // 未保存でないなら上書き済みのはず
			[['a'], ['a'], true, false], // 触ったが同じ内容に戻した
			[['a', 'b'], ['b', 'a'], true, false], // 並びが違うだけ
			[['a'], ['b'], true, true],
			[['a'], [], true, true],
			[[], ['a'], true, true]
		];
		for (const [current, server, unsaved, expected] of table) {
			expect(
				showsServerSelectionDiff(current, server, unsaved),
				`${JSON.stringify(current)} vs ${JSON.stringify(server)} (unsaved=${unsaved})`
			).toBe(expected);
		}
	});
});

describe('nextSelectionUnsaved', () => {
	it('チェックを触ったら未保存、保存・明示的な破棄・切断で降りる（総当たり）', () => {
		const expected: Record<SelectionEvent, boolean> = {
			edited: true,
			saved: false,
			discarded: false,
			disconnected: false
		};
		for (const current of [false, true]) {
			for (const event of Object.keys(expected) as SelectionEvent[]) {
				expect(nextSelectionUnsaved(current, event), `${current} + ${event}`).toBe(expected[event]);
			}
		}
	});
});

// --- レビュー P2-3: 応答が返らない相手でポーリングが止まらないこと ----------
//
// 「2 回連続で失敗したら取得できていないと言う」は**即座に失敗する障害にしか
// 成立していなかった**。TCP は繋がるが応答が返らない相手だと `catch` に入らず、
// 失敗も数えられず、`pollThenSchedule()` の次の予約も行われない。ここでの
// テストは **`vi.useFakeTimers()` と「解決しない Promise」**で、その障害を
// 再現する（即座に reject するモックでは一生検出できない）。

describe('readSubscriptionWithLimit / pollFailureOutcome', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	/** 解決も reject もしない読み取り（応答が返らない相手）。 */
	function neverSettles(): {
		read: (signal: AbortSignal) => Promise<HubSubscription>;
		signal: () => AbortSignal | null;
		resolveLate: (value: HubSubscription) => void;
	} {
		let captured: AbortSignal | null = null;
		let settle: ((value: HubSubscription) => void) | null = null;
		const pending = new Promise<HubSubscription>((resolve) => {
			settle = resolve;
		});
		return {
			read: (signal) => {
				captured = signal;
				return pending;
			},
			signal: () => captured,
			resolveLate: (value) => settle?.(value)
		};
	}

	it('上限を過ぎても返ってこない読み取りは timedOut になる（ここで戻るからループが続く）', async () => {
		vi.useFakeTimers();
		const { read, signal } = neverSettles();
		const pending = readSubscriptionWithLimit(read, SUBSCRIPTION_POLL_TIMEOUT_MS);

		// 上限の手前では、まだ何も決まっていない。
		await vi.advanceTimersByTimeAsync(SUBSCRIPTION_POLL_TIMEOUT_MS - 1);
		let settled = false;
		void pending.then(() => {
			settled = true;
		});
		await Promise.resolve();
		expect(settled).toBe(false);

		await vi.advanceTimersByTimeAsync(1);
		expect((await pending).kind).toBe('timedOut');
		// REST 経路は実際に往復を畳む（Tauri の invoke は中断できない）。
		expect(signal()?.aborted).toBe(true);
	});

	it('timedOut は失敗として数えられ、見出しと注記が「取得できていません」になる', async () => {
		vi.useFakeTimers();
		let failures = 0;
		// 2 周期とも応答が返らない相手。
		for (let attempt = 0; attempt < SUBSCRIPTION_POLL_FAILURE_LIMIT; attempt += 1) {
			const { read } = neverSettles();
			const pending = readSubscriptionWithLimit(read, SUBSCRIPTION_POLL_TIMEOUT_MS);
			await vi.advanceTimersByTimeAsync(SUBSCRIPTION_POLL_TIMEOUT_MS);
			const outcome = await pending;
			expect(outcome.kind).toBe('timedOut');
			failures = nextPollFailureCount(failures, pollFailureOutcome(outcome));
		}
		expect(failures).toBe(SUBSCRIPTION_POLL_FAILURE_LIMIT);
		expect(isSubscriptionStale(failures)).toBe(true);
		// 画面に出るところまで繋げて固定する（嘘の「受信中」で止まらないこと）。
		expect(hubSubscriptionHeadline('live', isSubscriptionStale(failures))).toContain(
			'状態を取得できていません'
		);
		expect(hubPollStaleNote(1722758400123)).toContain('取得できていません');
	});

	it('打ち切ったあとに遅れて解決しても、新鮮扱いには戻さない', async () => {
		vi.useFakeTimers();
		const { read, resolveLate } = neverSettles();
		const pending = readSubscriptionWithLimit(read, SUBSCRIPTION_POLL_TIMEOUT_MS);
		await vi.advanceTimersByTimeAsync(SUBSCRIPTION_POLL_TIMEOUT_MS);
		expect((await pending).kind).toBe('timedOut');

		// Tauri の invoke は中断できないので、打ち切ったあとに解決しうる。
		resolveLate(subscription({ state: 'live' }));
		await vi.advanceTimersByTimeAsync(1000);
		// 遅れた応答は ok にならない = 呼び出し側が markSubscriptionFresh() を
		// 呼ばず、嘘の表示が「最新」に戻らない。
		expect((await pending).kind).toBe('timedOut');
	});

	it('上限内に返れば従来どおり ok、reject は failed（打ち切りとは別の値）', async () => {
		vi.useFakeTimers();
		const live = subscription({ state: 'live' });
		const ok = await readSubscriptionWithLimit(async () => live, SUBSCRIPTION_POLL_TIMEOUT_MS);
		expect(ok).toEqual({ kind: 'ok', value: live });
		expect(pollFailureOutcome(ok)).toBe('ok');

		const failed = await readSubscriptionWithLimit(async () => {
			throw new Error('boom');
		}, SUBSCRIPTION_POLL_TIMEOUT_MS);
		expect(failed.kind).toBe('failed');
		expect(pollFailureOutcome(failed)).toBe('failed');
		expect(pollFailureOutcome({ kind: 'timedOut' })).toBe('failed');
	});

	it('上限は 1 回の読み取りに対するもので、ポーリング間隔より長い', () => {
		// 2 秒間隔のメモリ読み取りで、2 周期返らなければ応答していない。
		expect(SUBSCRIPTION_POLL_TIMEOUT_MS).toBe(4000);
	});
});

// --- 明示操作の上限（`HUB_UI_TIMEOUT_MS`）----------------------------------
//
// 購読ポーリングに入れた上限を明示操作にも広げたぶん。ポーリングの欠陥は
// 「黙って古い値を出し続ける」だったが、明示操作にはもっと重い症状がある:
// `run()` が `busy = true` のまま戻らず、**画面そのものが操作不能になる**
// （復旧はアプリの再起動のみ）。ここでも `vi.useFakeTimers()` と「解決しない
// Promise」で固定する。

describe('runWithLimit（明示操作にも使う汎用の上限）', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	/** 解決も reject もしない往復（応答が返らないアプリ）。 */
	function neverSettles<T>(): {
		run: (signal: AbortSignal) => Promise<T>;
		signal: () => AbortSignal | null;
		resolveLate: (value: T) => void;
	} {
		let captured: AbortSignal | null = null;
		let settle: ((value: T) => void) | null = null;
		const pending = new Promise<T>((resolve) => {
			settle = resolve;
		});
		return {
			run: (signal) => {
				captured = signal;
				return pending;
			},
			signal: () => captured,
			resolveLate: (value) => settle?.(value)
		};
	}

	it('上限を過ぎても返ってこない操作は timedOut になる（ここで戻るから busy が降りる）', async () => {
		vi.useFakeTimers();
		const { run, signal } = neverSettles<void>();
		const pending = runWithLimit(run, HUB_UI_TIMEOUT_MS);

		// 上限の手前では、まだ何も決まっていない = 正当に遅い操作を見限らない。
		await vi.advanceTimersByTimeAsync(HUB_UI_TIMEOUT_MS - 1);
		let settled = false;
		void pending.then(() => {
			settled = true;
		});
		await Promise.resolve();
		expect(settled).toBe(false);

		await vi.advanceTimersByTimeAsync(1);
		expect((await pending).kind).toBe('timedOut');
		// REST 経路は実際に往復を畳む（Tauri の invoke は中断できない）。
		expect(signal()?.aborted).toBe(true);
	});

	it('打ち切ったあとに遅れて解決しても ok にはしない（画面を書き換えさせない）', async () => {
		vi.useFakeTimers();
		const { run, resolveLate } = neverSettles<string>();
		const pending = runWithLimit(run, HUB_UI_TIMEOUT_MS);
		await vi.advanceTimersByTimeAsync(HUB_UI_TIMEOUT_MS);
		expect((await pending).kind).toBe('timedOut');

		resolveLate('遅れて届いた HubView');
		await vi.advanceTimersByTimeAsync(1000);
		expect((await pending).kind).toBe('timedOut');
	});

	it('上限内に解決すれば通常どおり ok、reject は失敗の中身を運ぶ failed', async () => {
		vi.useFakeTimers();
		const ok = await runWithLimit(async () => 'view', HUB_UI_TIMEOUT_MS);
		expect(ok).toEqual({ kind: 'ok', value: 'view' });

		const boom = new Error('boom');
		const failed = await runWithLimit(async () => {
			throw boom;
		}, HUB_UI_TIMEOUT_MS);
		// 明示操作は既存のエラー文言（`errorMessage()`）をそのまま出せること。
		expect(failed).toEqual({ kind: 'failed', error: boom });
	});

	it('購読側は同じ上限ヘルパの薄い包みで、エラーの中身だけ落とす', async () => {
		// 一般化で購読側の戻り値の形（`error` を持たない）を変えていないこと。
		const failed = await readSubscriptionWithLimit(async () => {
			throw new Error('boom');
		}, SUBSCRIPTION_POLL_TIMEOUT_MS);
		expect(failed).toEqual({ kind: 'failed' });
	});

	it('90秒はバックエンドの上限（60秒の connect + 順番待ちの読み取り15秒）を十分に超える', () => {
		// `hub.rs`: HUB_MUTATING_TIMEOUT = 60s、HUB_OPERATION_TIMEOUT = 15s。
		// 全操作が `begin_operation()` の操作ロックに並ぶので、正当に遅い最長は
		// 60 + 15 ≒ 75 秒。発火が「遅い」ではなく「応答していない」を意味する
		// 値であること（読み取り用と変更用に分けないのは、読み取りも同じ
		// 操作ロックに並ぶため）。
		expect(HUB_UI_TIMEOUT_MS).toBe(90000);
		expect(HUB_UI_TIMEOUT_MS).toBeGreaterThan((60 + 15) * 1000);
		// ポーリングの上限とは別物（あちらは 1 回のメモリ読み取り）。
		expect(HUB_UI_TIMEOUT_MS).toBeGreaterThan(SUBSCRIPTION_POLL_TIMEOUT_MS);
	});

	it('自動の読み直し専用の上限（15秒）は、明示操作の上限（90秒）より短い（#400）', () => {
		// `rereadAfterAbandon()` に来た時点で、直前の操作は既に 90 秒応答して
		// いない（＝固まっている）ことが確定しているので、もう一度 90 秒待つ
		// 意味が無い。逆に「正当に遅いだけの最長」（60 + 15 ≒ 75 秒、上の
		// テスト参照）は 90 秒の上限に届かないので、この経路には来ない -
		// 15 秒に縮めても正常な応答を見限らない。
		//
		// **どちらの定数がどちらの呼び出し（`rereadAfterAbandon` /
		// `reconfirmStatus`）に渡っているかは、この関係だけでは固定できない**
		// - `HubSection.svelte` 側の配線であって、純関数の入出力には出ない。
		expect(HUB_UI_REREAD_TIMEOUT_MS).toBe(15000);
		expect(HUB_UI_REREAD_TIMEOUT_MS).toBeLessThan(HUB_UI_TIMEOUT_MS);
	});
});

describe('hubAbandonedDisplay', () => {
	it('打ち切ったか × 状態を読み直せたか の総当たり', () => {
		const table: {
			abandoned: boolean;
			statusReread: boolean;
			notice: 'なし' | '読み直せた' | '読み直せない';
			applyStatus: boolean;
			blocksChanges: boolean;
		}[] = [
			{
				abandoned: false,
				statusReread: false,
				notice: 'なし',
				applyStatus: false,
				blocksChanges: false
			},
			{
				abandoned: false,
				statusReread: true,
				notice: 'なし',
				applyStatus: false,
				blocksChanges: false
			},
			{
				abandoned: true,
				statusReread: true,
				notice: '読み直せた',
				applyStatus: true,
				blocksChanges: false
			},
			{
				abandoned: true,
				statusReread: false,
				notice: '読み直せない',
				applyStatus: false,
				blocksChanges: true
			}
		];
		for (const row of table) {
			const display = hubAbandonedDisplay(row.abandoned, row.statusReread);
			const label = `abandoned=${row.abandoned} reread=${row.statusReread}`;
			expect(display.applyStatus, label).toBe(row.applyStatus);
			// オーナーレビュー P2-1: 状態を反映できたかどうかが、そのまま
			// 「変更操作を止めるか」になる（止めないと、接続先を確認できない
			// まま「選択を保存」が別の Hub の購読設定を書き換えうる）。
			expect(display.blocksChanges, label).toBe(row.blocksChanges);
			expect(display.blocksChanges, label).toBe(row.abandoned && !display.applyStatus);
			if (row.notice === 'なし') {
				expect(display.notice, label).toBeNull();
				continue;
			}
			expect(display.notice, label).not.toBeNull();
			// 打ち切ったときは**必ず**「操作は続いている可能性があります」。
			// これが無いと、非冪等な `connect`/`adoptHubKey` で Hub 側に残った
			// キーの存在が利用者に見えなくなる。
			expect(display.notice, label).toContain('操作は続いている可能性があります');
			// 「失敗しました」と言い切らない（打ち切ったのは待ち時間だけ）。
			expect(display.notice, label).not.toContain('失敗');
			// 上限は定数から出す（値を変えても文言とずれない）。
			expect(display.notice, label).toContain(`${HUB_UI_TIMEOUT_MS / 1000}秒`);
		}
	});

	it('読み直せたときと読み直せなかったときで文言が別になる（表示の鮮度が違う）', () => {
		const recovered = hubAbandonedDisplay(true, true).notice;
		const unknown = hubAbandonedDisplay(true, false).notice;
		expect(recovered).not.toBe(unknown);
		expect(recovered).toContain('読み直した現在の状態');
		expect(unknown).toContain('最新ではありません');
	});

	it('止めるときの文言は「何ができないか」と「何を押せばよいか」を含む', () => {
		// オーナーレビュー P2-1: 警告だけ出して操作を止めないと、警告がこの操作を
		// 止める役割を果たさない。止めた以上、抜け道も同じ行に書く。
		const blocked = hubAbandonedDisplay(true, false);
		expect(blocked.blocksChanges).toBe(true);
		for (const stopped of ['接続', '切断', '採用', '一覧の更新', '選択の保存', 'タグの選択']) {
			expect(blocked.notice, stopped).toContain(stopped);
		}
		expect(blocked.notice).toContain('状態を再取得');
	});

	it('読み直せたときは止めない（画面全体を操作不能にしない）', () => {
		expect(hubAbandonedDisplay(true, true).blocksChanges).toBe(false);
		expect(hubAbandonedDisplay(true, true).notice).not.toContain('止めています');
	});
});

// --- オーナーレビュー P2-1: 状態を再確認できるまで変更操作を止める ----------

describe('nextStatusUnconfirmed', () => {
	it('立っているか × 読み直せたか の総当たり', () => {
		const table: [boolean, StatusRereadOutcome, boolean][] = [
			// [今のフラグ, 「状態を再取得」の結末, 次のフラグ]
			[true, 'ok', false], // 読めた = 回復（`applyView()` で反映できた）
			[true, 'failed', true], // 読めなければ保ったまま（何も壊さない）
			[false, 'ok', false],
			[false, 'failed', false] // 打ち切っていないただの失敗では立てない
		];
		for (const [current, reread, expected] of table) {
			expect(nextStatusUnconfirmed(current, reread), `${current} + ${reread}`).toBe(expected);
		}
	});

	it('回復は「読めた」ときだけ。失敗を何回重ねても勝手には降りない', () => {
		let unconfirmed = hubAbandonedDisplay(true, false).blocksChanges;
		expect(unconfirmed).toBe(true);
		for (let attempt = 0; attempt < 3; attempt += 1) {
			unconfirmed = nextStatusUnconfirmed(unconfirmed, 'failed');
			expect(unconfirmed).toBe(true);
		}
		unconfirmed = nextStatusUnconfirmed(unconfirmed, 'ok');
		expect(unconfirmed).toBe(false);
	});
});

// --- #400 レビュー対応の仕上げ: 「状態を再取得」が失敗したら、失敗したと出す ---
//
// 以前は `outcome.kind !== 'ok'` を一律 `failed` として `nextStatusUnconfirmed`
// に渡すだけで、失敗時の表示は何も変わらなかった（押しても無反応に見えた）。
// `reconfirmStatusFailureNotice` が `timedOut`/`failed` を別の文言にする。

describe('reconfirmStatusFailureNotice', () => {
	it('ok / failed（理由あり） / timedOut の総当たり', () => {
		const table: {
			outcome: StatusRereadDetailedOutcome;
			errorText: string | null;
			expectNull: boolean;
		}[] = [
			{ outcome: 'ok', errorText: null, expectNull: true },
			{ outcome: 'failed', errorText: 'サーバーに接続できません', expectNull: false },
			{ outcome: 'timedOut', errorText: null, expectNull: false }
		];
		for (const row of table) {
			const notice = reconfirmStatusFailureNotice(row.outcome, row.errorText);
			if (row.expectNull) {
				expect(notice, row.outcome).toBeNull();
				continue;
			}
			expect(notice, row.outcome).not.toBeNull();
			// 押しても無反応に見えないこと（#400 レビュー対応の仕上げの主眼）:
			// 「再取得」自体に言及し、次の一手（もう一度押す）まで伝わる。
			expect(notice, row.outcome).toContain('再取得');
			expect(notice, row.outcome).toContain('もう一度');
			expect(notice, row.outcome).toContain('状態を再取得');
			// 止めている操作は引き続き列挙する（`hubAbandonedDisplay` と同じ規律）。
			for (const stopped of ['接続', '切断', '採用', '一覧の更新', '選択の保存', 'タグの選択']) {
				expect(notice, `${row.outcome} / ${stopped}`).toContain(stopped);
			}
		}
	});

	it('timedOut は「失敗」と言い切らない（打ち切りは操作の中止ではない）', () => {
		const notice = reconfirmStatusFailureNotice('timedOut', null);
		expect(notice).not.toBeNull();
		expect(notice).not.toContain('失敗');
		// 上限は定数から出す（値を変えても文言とずれない）。
		expect(notice).toContain(`${HUB_UI_TIMEOUT_MS / 1000}秒`);
	});

	it('failed はエラーが実際に返ってきているので「失敗」と言い切ってよい。理由を含む', () => {
		const notice = reconfirmStatusFailureNotice('failed', 'サーバーに接続できません');
		expect(notice).not.toBeNull();
		expect(notice).toContain('失敗');
		expect(notice).toContain('サーバーに接続できません');
	});

	it('failed と timedOut は別の文言になる', () => {
		const failed = reconfirmStatusFailureNotice('failed', '理由');
		const timedOut = reconfirmStatusFailureNotice('timedOut', null);
		expect(failed).not.toBe(timedOut);
	});
});

// --- オーナーレビュー P2-2: 保存本体と、保存後の購読読み取りの予算を分ける ---
//
// 以前は保存も読み直しも 1 つの `runWithLimit(action, HUB_UI_TIMEOUT_MS)` の
// 内側にあり、**保存が成功していても読み直しが返らないだけで操作全体が
// `timedOut`** になっていた（「保存しました」と「操作は続いている可能性が
// あります」が併存）。ここでも `vi.useFakeTimers()` と「解決しない Promise」で
// 固定する - 即座に reject するモックでは再現できない。

describe('saveSelectionWithLimits', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	/** 解決も reject もしない購読読み取り（応答が返らないアプリ）。 */
	function neverSettlingSubscription(): (signal: AbortSignal) => Promise<HubSubscription> {
		return () => new Promise<HubSubscription>(() => {});
	}

	function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
		let resolve!: (value: T) => void;
		const promise = new Promise<T>((res) => {
			resolve = res;
		});
		return { promise, resolve };
	}

	it('保存が成功すれば、購読の読み直しが返らなくても保存は ok のまま', async () => {
		vi.useFakeTimers();
		const save = deferred<void>();
		const onSaved = vi.fn();
		const pending = saveSelectionWithLimits(
			() => save.promise,
			neverSettlingSubscription(),
			onSaved
		);

		save.resolve();
		await vi.advanceTimersByTimeAsync(0);
		// 保存本体が成功した時点で「保存しました」を出す（読み直しを待たない）。
		expect(onSaved).toHaveBeenCalledTimes(1);

		await vi.advanceTimersByTimeAsync(SUBSCRIPTION_POLL_TIMEOUT_MS);
		const outcome = await pending;
		expect(outcome.save.kind).toBe('ok');
		expect(outcome.subscriptionReread).toEqual({ kind: 'timedOut' });
		// 打ち切り通知には進めない（`run()` 側の分岐は save の結末だけを見る）。
		expect(hubAbandonedDisplay(outcome.save.kind === 'timedOut', false).notice).toBeNull();
	});

	it('保存本体が上限直前に成功しても同じ（読み直しの 4 秒は全体の枠外）', async () => {
		// 「読み直しに 4 秒上限を足して全体 90 秒の枠内に残す」では塞げない境界。
		vi.useFakeTimers();
		const save = deferred<void>();
		const onSaved = vi.fn();
		const pending = saveSelectionWithLimits(
			() => save.promise,
			neverSettlingSubscription(),
			onSaved
		);

		await vi.advanceTimersByTimeAsync(HUB_UI_TIMEOUT_MS - 1);
		save.resolve();
		await vi.advanceTimersByTimeAsync(0);
		expect(onSaved).toHaveBeenCalledTimes(1);

		// ここから購読の 4 秒。合計は 90 秒を超えるが、保存の成否はもう確定済み。
		await vi.advanceTimersByTimeAsync(SUBSCRIPTION_POLL_TIMEOUT_MS);
		const outcome = await pending;
		expect(outcome.save.kind).toBe('ok');
		expect(outcome.subscriptionReread?.kind).toBe('timedOut');
	});

	it('読み直しの失敗・打ち切りは購読状態の取得失敗として数える', async () => {
		vi.useFakeTimers();
		const pendingTimedOut = saveSelectionWithLimits(
			async () => {},
			neverSettlingSubscription(),
			() => {}
		);
		await vi.advanceTimersByTimeAsync(SUBSCRIPTION_POLL_TIMEOUT_MS);
		const timedOut = await pendingTimedOut;
		expect(timedOut.save.kind).toBe('ok');
		expect(timedOut.subscriptionReread).toEqual({ kind: 'timedOut' });

		const failed = await saveSelectionWithLimits(
			async () => {},
			async () => {
				throw new Error('boom');
			},
			() => {}
		);
		expect(failed.save.kind).toBe('ok');
		expect(failed.subscriptionReread).toEqual({ kind: 'failed' });

		// 既存のポーリングと同じカウンタに合流する（別の数え方を作らない）。
		let failures = 0;
		for (const reread of [timedOut.subscriptionReread, failed.subscriptionReread]) {
			if (reread === null) continue;
			failures = nextPollFailureCount(failures, pollFailureOutcome(reread));
		}
		expect(failures).toBe(SUBSCRIPTION_POLL_FAILURE_LIMIT);
		expect(isSubscriptionStale(failures)).toBe(true);
	});

	it('保存が打ち切られたら「保存しました」も出さず、読み直しにも進まない', async () => {
		vi.useFakeTimers();
		const onSaved = vi.fn();
		const readSubscription = vi.fn(neverSettlingSubscription());
		const pending = saveSelectionWithLimits(
			() => new Promise<void>(() => {}),
			readSubscription,
			onSaved
		);
		await vi.advanceTimersByTimeAsync(HUB_UI_TIMEOUT_MS);
		const outcome = await pending;
		expect(outcome.save.kind).toBe('timedOut');
		expect(outcome.subscriptionReread).toBeNull();
		expect(onSaved).not.toHaveBeenCalled();
		expect(readSubscription).not.toHaveBeenCalled();
	});

	it('保存が失敗したら失敗の中身を運び、読み直しには進まない', async () => {
		const boom = new Error('boom');
		const onSaved = vi.fn();
		const readSubscription = vi.fn(neverSettlingSubscription());
		const outcome = await saveSelectionWithLimits(
			async () => {
				throw boom;
			},
			readSubscription,
			onSaved
		);
		expect(outcome.save).toEqual({ kind: 'failed', error: boom });
		expect(outcome.subscriptionReread).toBeNull();
		expect(onSaved).not.toHaveBeenCalled();
		expect(readSubscription).not.toHaveBeenCalled();
	});
});
