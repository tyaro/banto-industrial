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
	applyServerSelection,
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
	isPollGenerationCurrent,
	isPollResultFresh,
	isSubscriptionStale,
	nextPollFailureCount,
	nextSelectionUnsaved,
	pollFailureOutcome,
	readSubscriptionWithLimit,
	sameSelection,
	SELECTION_DISCARDED_NOTICE,
	showsServerSelectionDiff,
	needsManualKey,
	showManualKeyEntry,
	SUBSCRIPTION_POLL_FAILURE_LIMIT,
	SUBSCRIPTION_POLL_TIMEOUT_MS,
	type HubStatus,
	type HubSubscription,
	type HubSubscriptionState,
	type SelectionEvent
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
