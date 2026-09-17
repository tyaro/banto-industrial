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
import { describe, expect, it } from 'vitest';
import {
	hubStatusDetail,
	hubStatusLabel,
	hubSubscriptionDetail,
	hubSubscriptionLabel,
	hubLastValueLabel,
	hubTimeLabel,
	hubUnreachableCauseLabel,
	needsManualKey,
	showManualKeyEntry,
	type HubStatus,
	type HubSubscription,
	type HubSubscriptionState
} from './hubAdmin';

const ALL_STATES: HubStatus[] = [
	{ state: 'notConfigured' },
	{ state: 'connected', tagCount: 3 },
	{ state: 'authFailed' },
	{ state: 'forbidden' },
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

describe('hubLastValueLabel / hubTimeLabel', () => {
	it('まだ一度も受信していないことを明示する（空欄や 0 に潰さない）', () => {
		expect(hubLastValueLabel(null)).toBe('まだ受信していません');
	});

	it('受信済みなら epoch ミリ秒をその端末の書式で出す', () => {
		const epochMs = 1722758400123;
		expect(hubLastValueLabel(epochMs)).toBe(new Date(epochMs).toLocaleString());
		// epoch ミリ秒をそのまま数字で出さない。
		expect(hubLastValueLabel(epochMs)).not.toBe(String(epochMs));
	});

	it('解釈できない値は握りつぶさずそのまま見せる', () => {
		expect(hubTimeLabel(Number.NaN)).toBe('NaN');
	});
});
