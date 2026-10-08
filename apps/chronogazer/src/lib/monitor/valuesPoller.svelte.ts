/**
 * 監視画面（R1-D）の現在値ポーラー（`collect_values` / `GET /api/collect/values`）。
 *
 * ポーリングの規律は `routes/(app)/settings/CollectSection.svelte`（C-3b）に
 * 倣う。発明はしていない（docs/implementation-checklist.md §5「バックグラウンドの
 * 見張り・ポーリングを足したら」の 5 点）:
 *
 * - **二重起動の防止**: 同じ対象で動いている間の `start()` は周期を差し替える
 *   だけで、ループを増やさない。
 * - **停止条件**: 画面を離れたら（`onDestroy`）・タブが隠れたら・描かない種別に
 *   なったら、画面が `stop()` を呼ぶ。
 * - **停止と再開の競合**: 停止のたびに世代番号を進める。飛行中だった要求は送信時の
 *   世代を持ち、**応答の適用も次回の予約も**世代が現役のときしかしない
 *   （`isPollGenerationCurrent`）。これが無いと、停止した瞬間に飛んでいた要求が
 *   次のタイマを張り、再開後のループと二重に回る。
 * - **応答が返ってこない障害**: 1 回の読み取りに上限（`COLLECT_READ_TIMEOUT_MS`）。
 *   `AbortSignal` を渡して REST はソケットを畳み、Tauri の `invoke` は
 *   `runWithLimit` の「打ち切り済み」で遅れた応答を採らない。上限が無いと失敗も
 *   数えられず、ループごと止まる。
 * - **飛行中の応答が新しい状態を巻き戻さない**: グループを切り替えると世代が進む
 *   ので、前のグループ向けに飛んでいた応答は捨てる。**値はグループをまたいで
 *   持ち越さない**（切り替えた瞬間に前のグループの値を消し、「読み込み中」から
 *   始める）。
 *
 * 連続失敗は `COLLECT_POLL_FAILURE_LIMIT`（2）回で「取得できていない」に切り替え、
 * 最後に取得できた時刻を残す（`CollectSection.svelte` と同じ。表示は消さない）。
 * トーストは出さない。
 *
 * 判断（結果をどう状態に反映するか）は純関数 [`applyValuesOutcome`] に出し、
 * `valuesPoller.test.ts` が表で固定する。
 */
import {
	COLLECT_READ_TIMEOUT_MS,
	isCollectStale,
	isPollGenerationCurrent,
	nextPollFailureCount,
	runWithLimit,
	type CurrentSampleView,
	type Readout,
	type RunWithLimitOutcome
} from '../banto/collectAdmin';
import type { ValuesPhase } from './monitorLogic';

export type ValuesMap = Record<string, CurrentSampleView>;
export type ValuesReadout = Readout<ValuesMap>;
export type ValuesFetch = (signal: AbortSignal) => Promise<ValuesReadout>;

/** ポーラーが持つ状態（[`applyValuesOutcome`] の入出力）。 */
export interface ValuesState {
	phase: ValuesPhase;
	/** `phase === 'ready'` のときの値。それ以外は `null`。 */
	values: ValuesMap | null;
	/** 連続失敗回数（成功で 0）。 */
	failures: number;
	/** 最後に取得できた時刻（epoch ミリ秒）。 */
	lastOkAt: number | null;
}

export const INITIAL_VALUES_STATE: ValuesState = {
	phase: 'loading',
	values: null,
	failures: 0,
	lastOkAt: null
};

/**
 * 読み取り 1 回の結末を状態に反映する（純関数）。
 *
 * | 結末 | 反映 |
 * | --- | --- |
 * | `ok` + `ready` | 値を差し替え、失敗を 0 に、時刻を記録 |
 * | `ok` + `notRunning` | 「収集が動いていない」。値は捨てる（動いていないのに古い値を出さない）。失敗は 0 |
 * | `ok` + `unavailable` | 読めなかった。失敗を 1 足し、表示は残す（0 件に潰さない） |
 * | `failed` / `timedOut` | 失敗を 1 足し、表示は残す |
 */
export function applyValuesOutcome(
	state: ValuesState,
	outcome: RunWithLimitOutcome<ValuesReadout>,
	now: number
): ValuesState {
	if (outcome.kind === 'ok') {
		const readout = outcome.value;
		if (readout.state === 'ready') {
			return { phase: 'ready', values: readout.data, failures: 0, lastOkAt: now };
		}
		if (readout.state === 'notRunning') {
			return { phase: 'notRunning', values: null, failures: 0, lastOkAt: now };
		}
	}
	return { ...state, failures: nextPollFailureCount(state.failures, 'failed') };
}

/**
 * 収集が動いていない間の周期の下限（ms）。`CollectSection.svelte` の状態の
 * ポーリング（2 秒）と同じ。動いていない間はペンの周期（最短 500ms）で叩いても
 * 変わるものが無いので、間を空ける（無駄撃ちの抑制、チェックリスト §5）。収集が
 * 始まれば、この周期以内に値が出始める。
 */
export const NOT_RUNNING_POLL_MS = 2000;

/** 次の読み取りまでの間隔（純関数）。 */
export function nextPollDelayMs(phase: ValuesPhase, periodMs: number): number {
	return phase === 'notRunning' ? Math.max(periodMs, NOT_RUNNING_POLL_MS) : periodMs;
}

export interface ValuesPollerOptions {
	fetch: ValuesFetch;
	/** 1 回の読み取りの上限（既定 `COLLECT_READ_TIMEOUT_MS`）。 */
	timeoutMs?: number;
	now?: () => number;
}

export class ValuesPoller {
	/** 今の対象（表示グループの ID）。止まっていても最後の対象を残す。 */
	key = $state<number | null>(null);
	state = $state<ValuesState>(INITIAL_VALUES_STATE);

	readonly #fetch: ValuesFetch;
	readonly #timeoutMs: number;
	readonly #now: () => number;
	#generation = 0;
	#running = false;
	#periodMs = 1000;
	#timer: ReturnType<typeof setTimeout> | null = null;

	constructor(options: ValuesPollerOptions) {
		this.#fetch = options.fetch;
		this.#timeoutMs = options.timeoutMs ?? COLLECT_READ_TIMEOUT_MS;
		this.#now = options.now ?? Date.now;
	}

	/** 連続失敗が上限に達した（表示は最新ではない）。 */
	get stale(): boolean {
		return isCollectStale(this.state.failures);
	}

	/** 動いているか（テストと画面の確認用）。 */
	get running(): boolean {
		return this.#running;
	}

	/**
	 * `key` の対象でポーリングを始める。同じ対象で動いていれば周期だけ差し替える
	 * （次の予約から効く。二重に起動しない）。対象が変わったら値を捨てて始め直す。
	 */
	start(key: number, periodMs: number): void {
		this.#periodMs = periodMs;
		if (this.#running && this.key === key) return;
		this.stop();
		if (this.key !== key) {
			this.key = key;
			this.state = INITIAL_VALUES_STATE;
		}
		this.#running = true;
		void this.#loop(this.#generation);
	}

	/** 止める。飛行中の応答は世代が変わるので捨てられる。値は残す（同じ対象で再開したとき用）。 */
	stop(): void {
		this.#generation += 1;
		this.#running = false;
		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
	}

	async #loop(generation: number): Promise<void> {
		const outcome = await runWithLimit((signal) => this.#fetch(signal), this.#timeoutMs);
		if (!isPollGenerationCurrent(generation, this.#generation)) return;
		this.state = applyValuesOutcome(this.state, outcome, this.#now());
		this.#timer = setTimeout(
			() => {
				this.#timer = null;
				void this.#loop(generation);
			},
			nextPollDelayMs(this.state.phase, this.#periodMs)
		);
	}
}
