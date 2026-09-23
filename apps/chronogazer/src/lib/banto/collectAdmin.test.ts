/**
 * `collectAdmin.ts` の純関数に対するユニットテスト（#383 段階2b / R1-C の
 * C-3b）。`hubAdmin.test.ts` と同じ describe/it スタイルで、依存ゼロで直接
 * import できる範囲だけを固定する（`invoke`/`fetch` を伴う関数は E2E と
 * Rust 側のテストが担当する）。
 *
 * ここで固定したい核心は 5 つ:
 * 1. **5 状態がすべて別の文言になる**（`stopped`/`noTargets`/`startFailed` を
 *    「動いていない」で 1 つに潰さない）。
 * 2. **`Readout` の 3 つが別々に見える**（「読めなかった」を「0 件」や
 *    「接続なし」に潰さない）。
 * 3. **`pending`（受け付けたがまだ）・打ち切り・未受付のエラーを混ぜない**。
 * 4. **理由（`startFailed` の `reason`）は操作の結果としてだけ出る**
 *    （状態表示には無い）。
 * 5. 連続失敗の数え方と「取得できていません」への切り替え。
 */
import { describe, expect, it } from 'vitest';
import {
	COLLECT_POLL_FAILURE_LIMIT,
	COLLECT_UI_TIMEOUT_MS,
	collectActionLabel,
	collectConnectionsNote,
	collectEventsNote,
	collectOperationDisplay,
	collectStateDetail,
	collectStateHeadline,
	collectStateLabel,
	collectStaleNote,
	collectTimeLabel,
	connectionStatusLabel,
	eventKindLabel,
	isCollectStale,
	nextPollFailureCount,
	startFailedReason,
	toCollectStateView,
	type CollectAction,
	type CollectOutcome,
	type CollectorState,
	type CollectorStateView,
	type ConnectionStatusView,
	type ReadoutState,
	type RunWithLimitOutcome
} from './collectAdmin';

/**
 * `crates/banto-collect/src/event.rs` の `EventKind::as_str` が返す全 11 種を
 * ここに列挙して固定する（#415）。**Rust 側に種類を足したらここも足す** -
 * 足し忘れると `eventKindLabel` が未知の種類として生の綴りを返すだけで、
 * テストの失敗という形で気付ける。
 */
const EVENT_KINDS = [
	'collection_started',
	'collection_stopped',
	'plc_connected',
	'plc_disconnected',
	'plc_reconnected',
	'threshold_entered',
	'threshold_cleared',
	'clock_regression_entered',
	'clock_regression_cleared',
	'append_failure_entered',
	'append_failure_cleared'
] as const;

const ALL_STATES: CollectorStateView[] = [
	{ state: 'stopped' },
	{ state: 'starting' },
	{ state: 'running', groups: 2, tags: 10 },
	{ state: 'noTargets' },
	{ state: 'startFailed' }
];

const ALL_READOUTS: ReadoutState[] = ['notRunning', 'unavailable', 'ready'];

const ALL_ACTIONS: CollectAction[] = ['start', 'stop', 'restart'];

describe('collectStateLabel / collectStateDetail', () => {
	it('5状態がそれぞれ別の見出しになる（どれかが同じ表示に潰れない）', () => {
		const labels = ALL_STATES.map(collectStateLabel);
		expect(new Set(labels).size).toBe(ALL_STATES.length);
		expect(labels).not.toContain('');
	});

	it('5状態がそれぞれ別の補足説明になる', () => {
		const details = ALL_STATES.map(collectStateDetail);
		expect(new Set(details).size).toBe(ALL_STATES.length);
		expect(details).not.toContain('');
	});

	it('running は件数を見出しに出す', () => {
		expect(collectStateLabel({ state: 'running', groups: 1, tags: 3 })).toBe(
			'収集中（グループ1件 / タグ3件）'
		);
	});

	it('収集対象なしは「失敗」と言わない（別の状態として扱う）', () => {
		expect(collectStateLabel({ state: 'noTargets' })).not.toContain('失敗');
		expect(collectStateDetail({ state: 'noTargets' })).toContain('失敗ではありません');
	});

	it('startFailed の説明は理由を出さず、操作すると理由が返ることを案内する', () => {
		const detail = collectStateDetail({ state: 'startFailed' });
		expect(detail).toContain('この状態表示には含まれません');
		expect(detail).toContain(collectActionLabel('start'));
	});
});

describe('collectStateHeadline', () => {
	it('取得できていない間も状態名は残し、そのことだけを添える', () => {
		for (const state of ALL_STATES) {
			const label = collectStateLabel(state);
			expect(collectStateHeadline(state, false)).toBe(label);
			expect(collectStateHeadline(state, true)).toBe(`${label}（状態を取得できていません）`);
		}
	});
});

describe('isCollectStale / nextPollFailureCount', () => {
	it('連続失敗が上限に達するまで stale にならない', () => {
		expect(isCollectStale(0)).toBe(false);
		expect(isCollectStale(COLLECT_POLL_FAILURE_LIMIT - 1)).toBe(false);
		expect(isCollectStale(COLLECT_POLL_FAILURE_LIMIT)).toBe(true);
		expect(isCollectStale(COLLECT_POLL_FAILURE_LIMIT + 5)).toBe(true);
	});

	it('成功で 0 に戻る（1回の成功で通常表示へ復帰する）', () => {
		let count = 0;
		for (let i = 0; i < COLLECT_POLL_FAILURE_LIMIT + 2; i++) {
			count = nextPollFailureCount(count, 'failed');
		}
		expect(isCollectStale(count)).toBe(true);
		count = nextPollFailureCount(count, 'ok');
		expect(count).toBe(0);
		expect(isCollectStale(count)).toBe(false);
	});
});

describe('collectStaleNote', () => {
	it('2つの対象で別の文言になり、どちらも「いつの表示か」を出す', () => {
		const at = Date.parse('2026-09-21T10:00:00Z');
		const status = collectStaleNote('status', at);
		const connections = collectStaleNote('connections', at);
		expect(status).not.toBe(connections);
		expect(status).toContain(collectTimeLabel(at));
		expect(connections).toContain(collectTimeLabel(at));
	});

	it('まだ一度も取得できていない場合は、その旨を出す（時刻を偽らない）', () => {
		for (const subject of ['status', 'connections'] as const) {
			const note = collectStaleNote(subject, null);
			expect(note).toContain('まだ一度も取得できていません');
		}
	});
});

describe('collectConnectionsNote', () => {
	it('3つの結末がそれぞれ別の文言になる（読めなかったを0件に潰さない）', () => {
		const notes = ALL_READOUTS.map((state) => collectConnectionsNote(state, 0));
		expect(new Set(notes).size).toBe(ALL_READOUTS.length);
		expect(notes).not.toContain('');
	});

	it('「読めなかった」は0件とも失敗とも言わない', () => {
		const note = collectConnectionsNote('unavailable', 0);
		expect(note).toContain('読み取れませんでした');
		expect(note).not.toContain('0件');
		expect(note).toContain('失敗ではありません');
	});

	it('読めて0件は「事実として0件」と言い切る', () => {
		expect(collectConnectionsNote('ready', 0)).toContain('1件もありません');
		expect(collectConnectionsNote('ready', 3)).toContain('3件');
	});
});

describe('connectionStatusLabel', () => {
	it('3状態がそれぞれ別の文言になり、再接続は試行回数を残す', () => {
		const statuses: ConnectionStatusView[] = [
			{ status: 'connected' },
			{ status: 'reconnecting', attempt: 4 },
			{ status: 'stopped' }
		];
		const labels = statuses.map(connectionStatusLabel);
		expect(new Set(labels).size).toBe(statuses.length);
		expect(connectionStatusLabel({ status: 'reconnecting', attempt: 4 })).toContain('4回目');
	});
});

describe('collectEventsNote', () => {
	it('3つの結末がそれぞれ別の文言になる（読めなかったを空一覧に潰さない）', () => {
		const notes = ALL_READOUTS.map((state) => collectEventsNote(state, 0));
		expect(new Set(notes).size).toBe(ALL_READOUTS.length);
		expect(notes).not.toContain('');
	});

	it('「読めなかった」は0件と言わない', () => {
		expect(collectEventsNote('unavailable', 0)).toContain('0件ではありません');
	});

	it('読めたときは総件数と並び順を出す', () => {
		expect(collectEventsNote('ready', 0)).toContain('まだ1件も記録されていません');
		expect(collectEventsNote('ready', 1234)).toContain((1234).toLocaleString());
		expect(collectEventsNote('ready', 1234)).toContain('新しい順');
	});
});

describe('toCollectStateView / startFailedReason', () => {
	it('操作の応答から理由を取り出し、表示用の状態からは落とす', () => {
		const failed: CollectorState = { state: 'startFailed', reason: 'PLCに接続できません' };
		expect(startFailedReason(failed)).toBe('PLCに接続できません');
		expect(toCollectStateView(failed)).toEqual({ state: 'startFailed' });
	});

	it('理由を持たない4状態はそのまま通り、理由も無い', () => {
		const others: CollectorState[] = [
			{ state: 'stopped' },
			{ state: 'starting' },
			{ state: 'running', groups: 1, tags: 1 },
			{ state: 'noTargets' }
		];
		for (const state of others) {
			expect(toCollectStateView(state)).toEqual(state);
			expect(startFailedReason(state)).toBeNull();
		}
	});
});

describe('collectOperationDisplay', () => {
	const settled = (status: CollectorState): RunWithLimitOutcome<CollectOutcome> => ({
		kind: 'ok',
		value: { status, pending: false }
	});
	const accepted = (status: CollectorState): RunWithLimitOutcome<CollectOutcome> => ({
		kind: 'ok',
		value: { status, pending: true }
	});

	it('4つの結末（失敗・打ち切り・受付済み・完了）が3操作ともすべて別の出方になる', () => {
		for (const action of ALL_ACTIONS) {
			const outcomes: [string, RunWithLimitOutcome<CollectOutcome>, string | null][] = [
				['failed', { kind: 'failed', error: new Error('boom') }, 'エラーの文言'],
				['timedOut', { kind: 'timedOut' }, null],
				// **`pending` と `settled` に同じ状態を渡す**のが要点: 状態を
				// 変えてしまうと、`pending` の分岐を消しても「状態名が違うから
				// 別の文言」になって反証にならない（`pending` を「完了しました」に
				// 潰す退行をこのテストで捕まえられなくなる）。
				['pending', accepted({ state: 'running', groups: 1, tags: 2 }), null],
				['settled', settled({ state: 'running', groups: 1, tags: 2 }), null]
			];
			const rendered = outcomes.map(([, outcome, text]) =>
				collectOperationDisplay(action, outcome, text)
			);
			// 失敗だけが error 行、それ以外は notice 行（混ぜない）。
			expect(rendered.map((display) => display.error !== null)).toEqual([
				true,
				false,
				false,
				false
			]);
			expect(rendered.map((display) => display.notice !== null)).toEqual([false, true, true, true]);
			const texts = rendered.map((display) => display.error ?? display.notice);
			expect(new Set(texts).size).toBe(outcomes.length);
			for (const text of texts) expect(text).toContain(collectActionLabel(action));
		}
	});

	it('未受付（混雑）のエラーは backend の「実行されていません」をそのまま出す', () => {
		const message =
			'収集の操作を受け付けられませんでした（処理が混み合っています）。前の操作が終わるのを待って、もう一度お試しください（この操作は実行されていません）';
		const display = collectOperationDisplay('stop', { kind: 'failed', error: {} }, message);
		expect(display.error).toContain('この操作は実行されていません');
		// pending の文言（「受け付けました」）と混ざらない。
		expect(display.error).not.toContain('受け付けました');
		expect(display.notice).toBeNull();
	});

	it('pending は「完了した」と言わず、状態表示で追いつくことを伝える', () => {
		const display = collectOperationDisplay('start', accepted({ state: 'starting' }), null);
		expect(display.notice).toContain('受け付けました');
		expect(display.notice).toContain('まだ終わっていません');
		expect(display.notice).not.toContain('完了しました');
		expect(display.error).toBeNull();
	});

	it('打ち切りは「失敗」と言わず、操作が続いている可能性を伝える', () => {
		const display = collectOperationDisplay('restart', { kind: 'timedOut' }, null);
		expect(display.notice).toContain('待つのをやめただけ');
		expect(display.notice).toContain(`${Math.round(COLLECT_UI_TIMEOUT_MS / 1000)}秒`);
		expect(display.notice).not.toContain('失敗');
		expect(display.error).toBeNull();
	});

	it('完了したときは現在の状態を添え、理由があれば必ず出す（状態表示には無い情報）', () => {
		const withReason = collectOperationDisplay(
			'stop',
			settled({ state: 'startFailed', reason: 'data.dir を開けません' }),
			null
		);
		expect(withReason.notice).toContain('開始に失敗しました');
		expect(withReason.notice).toContain('data.dir を開けません');

		const withoutReason = collectOperationDisplay(
			'start',
			settled({ state: 'running', groups: 1, tags: 2 }),
			null
		);
		expect(withoutReason.notice).toContain('収集中');
		expect(withoutReason.notice).not.toContain('理由');
	});

	it('失敗の文言が無くても空にならない（理由不明として出す）', () => {
		const display = collectOperationDisplay('start', { kind: 'failed', error: null }, null);
		expect(display.error).toContain('理由不明');
	});
});

describe('collectTimeLabel', () => {
	it('不正な時刻でも例外を投げず、そのまま数値を返す', () => {
		expect(collectTimeLabel(Number.NaN)).toBe(String(Number.NaN));
	});
});

describe('eventKindLabel', () => {
	it('EventKind の全 11 種にラベルがある（空文字・原文のままは無い）', () => {
		for (const kind of EVENT_KINDS) {
			const label = eventKindLabel(kind);
			expect(label.length).toBeGreaterThan(0);
			expect(label).not.toBe(kind);
		}
	});

	it('対になるイベントは対になる文言にする（超過/復帰、失敗/復帰、逆行/復帰）', () => {
		expect(eventKindLabel('threshold_entered')).toBe('しきい値超過');
		expect(eventKindLabel('threshold_cleared')).toBe('しきい値復帰');
		expect(eventKindLabel('clock_regression_entered')).toBe('時刻逆行');
		expect(eventKindLabel('clock_regression_cleared')).toBe('時刻逆行復帰');
		expect(eventKindLabel('append_failure_entered')).toBe('書き込み失敗');
		expect(eventKindLabel('append_failure_cleared')).toBe('書き込み復帰');
	});

	it('未知の種類は綴りをそのまま出す（落とさない・失敗しない）', () => {
		expect(eventKindLabel('some_future_kind')).toBe('some_future_kind');
	});
});
