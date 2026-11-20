/**
 * ライブのトレンドの持ち主（監視画面・自由トレンドの画面の共通部品、#1207・#1231 PR 8b1）。
 *
 * 監視画面の `$effect` に書いてあった「現在値のポーラーの結果を `TrendFeed` へ流す」段取りを
 * そのまま切り出した（挙動は変えていない）:
 *
 * - **サーバーの時計**（`trendLogic.ts` の `observeServerClock`）: 現在値の応答の `serverNowMs` を
 *   基準に端末の単調な経過時間で進める。受け取るまでは `null` で、トレンドを作らない。時計は
 *   サーバー全体のものなので、対象（グループ・自由トレンド）を替えても残す。
 * - **途切れ**: 読み取りの失敗（連続失敗が増えた）・収集の停止・タブが隠れた間は
 *   `breakLive()` でリングバッファに途切れを知らせる。
 * - **対象の切り替え**: 前の対象の線は、新しい対象の値が来る前でも捨てる。
 *
 * 画面は `$effect` の中で依存する値を読み、`untrack` の中から [`LiveTrend.update`] を呼ぶ。
 */
import type { PenView } from './monitorLogic';
import { TrendFeed } from './trendFeed.svelte';
import { observeServerClock, serverClockNow, type ServerClock } from './trendLogic';
import type { ValuesState } from './valuesPoller.svelte';

/** トレンドの対象（表示グループのトレンド・自由トレンド）。`id` が `TrendFeed` のキー。 */
export interface LiveTrendTarget {
	id: number;
	pens: readonly { tagId: number }[];
}

export interface LiveTrendInput {
	target: LiveTrendTarget | null;
	/** ポーラーの今の状態（まだ無ければ `null`）。 */
	current: ValuesState | null;
	/** 描画域の幅（px）。 */
	width: number;
	windowSec: number;
	/** ポーリングの周期（ms。決まらなければ `null`）。 */
	periodMs: number | null;
	views: readonly Pick<PenView, 'value'>[];
	visible: boolean;
	/** タグの収集周期（ms。分からなければ `null`）。 */
	collectPeriodOf: (tagId: number) => number | null;
}

export class LiveTrend {
	readonly feed = new TrendFeed();
	private serverClock: ServerClock = null;
	/** 時計に反映済みの現在値のスナップショット（成功のたびに新しいオブジェクト）。 */
	private observedValues: object | null = null;
	/** 前回見た連続失敗の回数（増えたらリングバッファに途切れを知らせる）。 */
	private seenFailures = 0;

	update(input: LiveTrendInput): void {
		const { target, current, width, windowSec, periodMs, views, visible } = input;
		const feed = this.feed;
		if (target === null) {
			feed.reset();
			return;
		}
		// 対象が替わったら、新しい対象の値が来る前でも前の線を捨てる。
		if (feed.groupId !== null && feed.groupId !== target.id) feed.reset();
		// 途切れ: 読み取りの失敗（連続失敗が増えた）・収集の停止・タブが隠れた（ポーリングを
		// 止めた）。その間の値はリングバッファに無いので、履歴で描く。
		const failures = current?.failures ?? 0;
		if (!visible || current === null || current.phase !== 'ready' || failures > this.seenFailures) {
			feed.breakLive();
		}
		this.seenFailures = failures;
		if (current === null || current.phase !== 'ready' || current.values === null) return;
		if (current.values !== this.observedValues) {
			this.observedValues = current.values;
			this.serverClock = observeServerClock(
				this.serverClock,
				current.serverNowMs,
				performance.now()
			);
		}
		const now = serverClockNow(this.serverClock, performance.now());
		if (now === null || width <= 0) return;
		const penTagIds = target.pens.map((pen) => pen.tagId);
		feed.sync(
			{
				groupId: target.id,
				penTagIds,
				windowMs: windowSec * 1000,
				plotWidthPx: width,
				// サンプル間隔: タグの収集周期とポーリングの周期の長い方（どちらより短くは届かない）。
				sampleIntervalMs: penTagIds.map(
					(tagId) => Math.max(input.collectPeriodOf(tagId) ?? 0, periodMs ?? 0) || null
				)
			},
			now,
			current.values,
			views.map((view) => view.value)
		);
	}

	reset(): void {
		this.feed.reset();
	}
}
