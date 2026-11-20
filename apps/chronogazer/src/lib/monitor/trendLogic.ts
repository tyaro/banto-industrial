/**
 * 監視画面のトレンド（R1-D の D-3b、recorder-requirements.md §3.3 の「リアルタイム」。
 * 設計書 docs/chronogazer/trend-design.md §8・§10 の 4b でハイブリッドにした）の判断をまとめた
 * 純関数（`trendLogic.test.ts` が表で固定する）。データの持ち主（`trendFeed.svelte.ts`）と
 * パネル（`TrendPanel.svelte`）はここを呼ぶだけで、判断を書き写さない
 * （docs/implementation-checklist.md §5）。
 *
 * ## ハイブリッド（直近はリングバッファ、過去は履歴の区間）
 *
 * 描画は共通の部品 `TrendChart`（`@banto-industrial/trend`）で、データのつなぎ合わせは部品の
 * 純粋な核（`liveAppendAll`・`hybridData`・`boundaryPlan` ほか、設計書 §8）が持つ。ここに置くのは
 * 監視画面だけの判断:
 *
 * - **追従**（[`startFollow`]・[`tickFollow`]・[`rangeChangeFollow`]）: 追従中は右端を今にして
 *   幅を保って進める。拡大・縮小・パンで右端が今から外れたら追従をやめ、右端を今へ戻したら
 *   （`isAtNow`、[`FOLLOW_TOLERANCE_MS`] 以内）その幅で追従を再開する。「最新へ」・時間窓の
 *   選び直しは窓の幅で追従を始め直す。
 * - **履歴の読み直し**（[`needsHistoryReload`]）: 追従中に毎秒は読まない。区間の幅が変わった・
 *   表示範囲の左端が最後に要求した範囲の外に出た・途切れの後に履歴とライブの間に隙間ができた
 *   ときだけ読む（止めている間は、履歴で描く部分が要求した範囲を出たときも）。
 *
 * 時刻は**サーバーの時計**（現在値の応答の `serverNowMs`、履歴の `tMs`）で揃える。端末の時計を
 * 使うと、LAN の別の端末で時計がずれているとき履歴と現在値が食い違う。表示の時計は
 * [`observeServerClock`] / [`serverClockNow`]: **現在値の応答を作った時点のサーバーの時刻**
 * （`serverNowMs`）を基準にし、次の応答までは**端末の単調な経過時間**（`performance.now()`）で
 * 進める。成功した応答のたびに基準を付け替え、表示の時刻は戻さない（付け替えで見積もりより前に
 * なったら、追いつくまで止める）。
 *
 * - サンプルの `ptimeMs`（読みに行った時刻）を時計にすると、表示中のタグがすべて収集周期
 *   60 秒のとき `ptimeMs` が 60 秒間止まり、毎分線が切れた（PR #543 のオーナーレビュー P2）。
 *   応答の時刻はサンプルの古さに関係しないので、good のサンプルは常に「サーバーの今」に書かれる。
 * - **サーバーの時刻を受け取るまでは時計が無い**（`null`）。そのあいだリングバッファに書かず、
 *   履歴も要求しない（端末の時計で書くと、後から来たサーバーの時刻が「過去」に見えて捨てられる。
 *   PR #543 のレビュー P2）。
 * - 読み取りの失敗・収集の停止・タブが隠れた間は、リングバッファに途切れを知らせる
 *   （`liveBreakAll`。その範囲は履歴で描く）。`bad` / `stale` の値は `null`（欠測）のサンプル。
 *
 * ## 管理線（#1207・#1231 PR 7）
 *
 * 管理線は**ペンの属性**（`limitLines`。既定は無効）。引く値は、ペンの上書き → タグのしきい値
 * （[`resolveLimits`]）。描くのは部品（`TrendLimit`。越えた区間の色替えも部品）。
 */
import {
	drawableSeries,
	followRange,
	isAtNow,
	isValidRange,
	type TimeRange,
	type TrendData
} from '@banto-industrial/trend';
import type { Pen, PenStyle } from '../banto/displayGroupsAdmin';
import type { TagWithThresholds, ThresholdFields } from '../banto/tagThresholdsAdmin';
import { formatValue, isBitTag } from './monitorLogic';

// --- 時間窓 ------------------------------------------------------------------

/**
 * 選べる時間窓（秒）。Rust の `TREND_TIME_WINDOWS_SEC`、「表示グループ」ページの
 * `TIME_WINDOW_OPTIONS` と同じ（`trendLogic.test.ts` が突き合わせる）。
 */
export const TREND_WINDOWS_SEC: readonly number[] = [60, 300, 600, 1800, 3600];
/** グループに既定の窓が無い・不正なときの窓（R1-D「既定窓 10 分」）。 */
export const DEFAULT_TREND_WINDOW_SEC = 600;

export function isTrendWindowSec(value: unknown): value is number {
	return typeof value === 'number' && TREND_WINDOWS_SEC.includes(value);
}

/** 時間窓の表示名（「10 分」「1 時間」）。 */
export function trendWindowLabel(sec: number): string {
	if (sec % 3600 === 0) return `${sec / 3600} 時間`;
	if (sec % 60 === 0) return `${sec / 60} 分`;
	return `${sec} 秒`;
}

/** 端末ごとの時間窓（Q4）の localStorage のキー。値は `{ "<グループ ID>": 秒 }`。 */
export const TREND_WINDOW_STORAGE_KEY = 'chronogazer.monitor.trendWindowSec';

function readWindowMap(storage: Pick<Storage, 'getItem'>): Record<string, unknown> {
	const raw = storage.getItem(TREND_WINDOW_STORAGE_KEY);
	if (raw === null) return {};
	const parsed: unknown = JSON.parse(raw);
	return parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
		? (parsed as Record<string, unknown>)
		: {};
}

/** この端末で選んだ窓（秒）。無い・壊れている・選択肢に無いときは `null`。 */
export function loadTrendWindowOverride(
	storage: Pick<Storage, 'getItem'> | undefined,
	groupId: number
): number | null {
	if (!storage) return null;
	try {
		const value = readWindowMap(storage)[String(groupId)];
		return isTrendWindowSec(value) ? value : null;
	} catch {
		return null;
	}
}

/**
 * この端末の窓を覚える（ベストエフォート）。グループの既定と同じ窓を選んだら覚えを
 * 消す（以後はグループの既定に従う）。**グループの定義には書かない**（Q4）。
 */
export function saveTrendWindowOverride(
	storage: Pick<Storage, 'getItem' | 'setItem'> | undefined,
	groupId: number,
	sec: number,
	groupDefaultSec: number
): void {
	if (!storage || !isTrendWindowSec(sec)) return;
	try {
		let map: Record<string, unknown>;
		try {
			map = readWindowMap(storage);
		} catch {
			map = {};
		}
		const next = { ...map };
		if (sec === groupDefaultSec) delete next[String(groupId)];
		else next[String(groupId)] = sec;
		storage.setItem(TREND_WINDOW_STORAGE_KEY, JSON.stringify(next));
	} catch {
		// 端末の便利機能なので、失敗しても表示は続ける。
	}
}

/** グループの既定の窓（属性が無い・不正なら [`DEFAULT_TREND_WINDOW_SEC`]）。 */
export function groupDefaultWindowSec(attributes: { timeWindowSec?: number }): number {
	return isTrendWindowSec(attributes.timeWindowSec)
		? attributes.timeWindowSec
		: DEFAULT_TREND_WINDOW_SEC;
}

/** 使う窓: 端末の覚え → グループの既定。 */
export function resolveTrendWindowSec(
	attributes: { timeWindowSec?: number },
	override: number | null
): number {
	return override ?? groupDefaultWindowSec(attributes);
}

/**
 * タグの収集周期（ms。タグ → 収集グループの `periodMs`）。引けない・正でないときは `null`。
 * 監視画面がリングバッファのサンプル間隔（ポーリングの周期との長い方）に使う。
 */
export function collectPeriodMsOf(
	tags: readonly { id: number; collectionGroupId: number }[],
	collectionGroups: readonly { id: number; periodMs: number }[],
	tagId: number
): number | null {
	const tag = tags.find((t) => t.id === tagId);
	const cg = tag ? collectionGroups.find((g) => g.id === tag.collectionGroupId) : undefined;
	return cg && Number.isFinite(cg.periodMs) && cg.periodMs > 0 ? cg.periodMs : null;
}

/**
 * 表示の時計。`serverMs` は基準にしたサーバーの時刻（応答の `serverNowMs`）、`localMs` は
 * それを受け取ったときの端末の単調な時刻、`floorMs` は付け替えた時点で表示していた時刻
 * （表示の時刻をこれより前にしない）。`null` = まだサーバーの時刻を受け取っていない。
 */
export type ServerClock = { serverMs: number; localMs: number; floorMs: number } | null;

/** 時計の今（純関数）: 基準 + 端末の経過時間（戻さない）。時計が無ければ `null`。 */
export function serverClockNow(clock: ServerClock, localNowMs: number): number | null {
	if (clock === null) return null;
	return Math.max(clock.floorMs, clock.serverMs + Math.max(0, localNowMs - clock.localMs));
}

/**
 * 成功した現在値の応答を受け取ったときに時計を付け替える（純関数）。`serverNowMs` が
 * 無ければ（古い相手）時計はそのまま。表示の時刻は戻さない（`floorMs`）。
 */
export function observeServerClock(
	clock: ServerClock,
	serverNowMs: number | null,
	localNowMs: number
): ServerClock {
	if (serverNowMs === null || !Number.isFinite(serverNowMs)) return clock;
	const shown = serverClockNow(clock, localNowMs);
	return { serverMs: serverNowMs, localMs: localNowMs, floorMs: shown ?? serverNowMs };
}

// --- 追従と履歴の読み直し（ハイブリッド、設計書 §8・§10 の 4b） ---------------------

/** 右端が今からこれ以内なら「今に付いている」とみなし、追従を再開する。 */
export const FOLLOW_TOLERANCE_MS = 1000;

/** 追従の状態。`following` の間は右端を今にして、幅 `widthMs` のまま進める。 */
export interface FollowState {
	following: boolean;
	widthMs: number;
	/** 表示範囲（部品の半開区間）。 */
	view: TimeRange;
}

/** 追従を始める（構成を当てた・時間窓を替えた・「最新へ」）。幅は部品の範囲（1 分〜31 日）に丸める。 */
export function startFollow(widthMs: number, nowMs: number): FollowState {
	const view = followRange(widthMs, nowMs);
	return { following: true, widthMs: view.endMs - view.startMs, view };
}

/** 時計が進んだ。追従中なら右端を今にする。止めている間・変わらないときは同じオブジェクト。 */
export function tickFollow(state: FollowState, nowMs: number): FollowState {
	if (!state.following || !Number.isFinite(nowMs)) return state;
	const view = followRange(state.widthMs, nowMs);
	return view.startMs === state.view.startMs && view.endMs === state.view.endMs
		? state
		: { ...state, view };
}

/**
 * 拡大・縮小・パンの結果を当てる。右端が今に付いていれば（`isAtNow`）その幅で追従し、
 * 外れたら（左へパン・今から離れた位置での拡大）追従をやめて範囲をそのまま使う。
 * 不正な範囲は無視する。幅の上限は部品の `MAX_RANGE_MS`（拡大・縮小が丸める）。
 */
export function rangeChangeFollow(state: FollowState, next: TimeRange, nowMs: number): FollowState {
	if (!isValidRange(next)) return state;
	if (isAtNow(next, nowMs, FOLLOW_TOLERANCE_MS)) {
		return startFollow(next.endMs - next.startMs, nowMs);
	}
	return { following: false, widthMs: next.endMs - next.startMs, view: next };
}

/** 最後に出した履歴の要求（範囲と区間の幅）。 */
export interface HistoryRequested {
	range: TimeRange;
	binMs: number;
}

/**
 * 履歴を読み直すか（純関数）。追従中に毎秒は読まない（無駄撃ちしない）。
 *
 * ペンごとに、表示範囲のうち履歴で描く部分は `[view.startMs, liveFrom)`（リングバッファが無い
 * ペンは右端まで）。どのペンも履歴で描く部分が無ければ（表示範囲を全部リングバッファで描ける）、
 * 区間の幅が変わったとき以外は読まない。読むのは:
 *
 * 1. まだ要求していない。
 * 2. 区間の幅が変わった（拡大・縮小・描画域の幅）。リングバッファのまとめ方も履歴の幅で決まるため。
 * 3. 表示範囲の左端が、最後に要求した範囲の外にある（左へパン・縮小、追従で窓 1 つぶん進んだ）。
 * 4. あるペンの `liveFrom` が要求した範囲の終わりより後で、表示範囲の中にある（途切れの後に
 *    書き直し始めた。その間は履歴に記録があるかもしれない）。
 * 5. 追従を止めている間は、履歴で描く部分の終わりが要求した範囲の終わりより後。
 */
export function needsHistoryReload(input: {
	requested: HistoryRequested | null;
	view: TimeRange;
	binMs: number;
	following: boolean;
	/** ペンの並びどおりの `liveFrom`（リングバッファが無いペンは `null`）。 */
	liveFroms: readonly (number | null)[];
}): boolean {
	const { requested, view, liveFroms } = input;
	if (!isValidRange(view) || liveFroms.length === 0) return false;
	if (requested === null) return true;
	if (requested.binMs !== input.binMs) return true;
	const needEnd = Math.max(
		...liveFroms.map((f) => (f === null ? view.endMs : Math.min(view.endMs, f)))
	);
	if (!(needEnd > view.startMs)) return false;
	if (view.startMs < requested.range.startMs || view.startMs >= requested.range.endMs) return true;
	if (
		liveFroms.some(
			(f) => f !== null && f > requested.range.endMs && f > view.startMs && f < view.endMs
		)
	) {
		return true;
	}
	return !input.following && needEnd > requested.range.endMs;
}

// --- 管理線 ------------------------------------------------------------

export const TREND_WARNING_COLOR = 'var(--banto-warning)';
export const TREND_DANGER_COLOR = 'var(--banto-danger)';

/** 解決した管理線の値（各 `null` = 引かない）。 */
export interface ResolvedLimits {
	hh: number | null;
	h: number | null;
	l: number | null;
	ll: number | null;
}

/**
 * 管理線の値を決める（純関数）。`limitLines === true` で bit でないときだけ。各値 = ペンの上書き →
 * タグのしきい値（タグを読めていなければ上書きだけ）。4 つとも無ければ `null`。
 */
export function resolveLimits(
	pen: Partial<Pick<PenStyle, 'limitLines' | 'limitHh' | 'limitH' | 'limitL' | 'limitLl'>>,
	tag: Partial<ThresholdFields> | undefined,
	isBit: boolean
): ResolvedLimits | null {
	if (pen.limitLines !== true || isBit) return null;
	const limits: ResolvedLimits = {
		hh: pen.limitHh ?? tag?.thresholdHh ?? null,
		h: pen.limitH ?? tag?.thresholdH ?? null,
		l: pen.limitL ?? tag?.thresholdL ?? null,
		ll: pen.limitLl ?? tag?.thresholdLl ?? null
	};
	return limits.hh === null && limits.h === null && limits.l === null && limits.ll === null
		? null
		: limits;
}

// --- パネルの表示 -------------------------------------------------------------

/** パネルに渡すペン 1 本（判断は済ませてある）。 */
export interface TrendPenInfo {
	tagId: number;
	name: string;
	unit: string | null;
	/** 小数桁（タグを読めていなければ `null`）。 */
	decimals: number | null;
	/** bit のタグ（縦軸は 0〜1、小数桁は指定しない。凡例・カーソル・要約・縦軸の目盛りは False / True で出す）。 */
	isBit: boolean;
	/** bit の表し（`null` = 段差）。帯になるのは bit で `'band'` のペンだけ（部品が判断する）。 */
	bitStyle: 'step' | 'band' | null;
	/** 帯の太さ px（`null` = 部品の既定の 16）。 */
	bitBandHeight: number | null;
	colorSlot: number;
	/** ペンの属性の縦軸の側（`null` = 左）。bit でも効く。 */
	axis: 'left' | 'right' | null;
	/** ペンの属性の縦軸の最小・最大（`null` = 工学値レンジ）。bit のタグでは使わない（0〜1 固定）。 */
	yMin: number | null;
	yMax: number | null;
	/** タグの工学値レンジ（`engLo < engHi` で両方有限のときだけ。縦軸の指定の無い側に使う）。 */
	engRange: { min: number; max: number } | null;
	/** ペンの属性の線種・線幅・マーカー（`null` = 既定。効くのはトレンドだけ。bit でも効く）。 */
	lineStyle: PenStyle['lineStyle'];
	lineWidth: number | null;
	marker: PenStyle['marker'];
	markerSize: number | null;
	/** 引く管理線の値（`null` = 引かない。ペンの属性が無効・bit・値が無いとき）。 */
	limits: ResolvedLimits | null;
}

/** タグの工学値レンジ。両方が有限で `engLo < engHi` のときだけ返す。 */
export function tagEngRange(
	tag: { engLo?: number | null; engHi?: number | null } | undefined
): { min: number; max: number } | null {
	const lo = tag?.engLo;
	const hi = tag?.engHi;
	if (typeof lo !== 'number' || typeof hi !== 'number') return null;
	if (!Number.isFinite(lo) || !Number.isFinite(hi) || lo >= hi) return null;
	return { min: lo, max: hi };
}

/**
 * グループのペンをパネルの形にする（純関数）。タグを読めていなければ「タグ ID n」で
 * 単位・小数桁・管理線の値はペンの上書きだけ。
 */
export function trendPenInfos(
	group: { pens: readonly (Pick<Pen, 'tagId' | 'colorSlot'> & Partial<PenStyle>)[] },
	tags: readonly TagWithThresholds[]
): TrendPenInfo[] {
	return group.pens.map((pen, index) => {
		const tag = tags.find((t) => t.id === pen.tagId);
		const isBit = isBitTag(tag);
		return {
			tagId: pen.tagId,
			// ペンの属性（#1207・#1231 PR 6a）: 凡例の名前・単位・小数桁は、あればタグの値より優先。
			name: pen.label ?? tag?.name ?? `タグ ID ${pen.tagId}`,
			unit: pen.unit ?? (tag?.unit ? tag.unit : null),
			decimals: pen.decimals ?? (tag ? tag.decimals : null),
			isBit,
			colorSlot: pen.colorSlot ?? index + 1,
			axis: pen.axis ?? null,
			yMin: pen.yMin ?? null,
			yMax: pen.yMax ?? null,
			engRange: tagEngRange(tag),
			lineStyle: pen.lineStyle ?? null,
			lineWidth: pen.lineWidth ?? null,
			marker: pen.marker ?? null,
			markerSize: pen.markerSize ?? null,
			bitStyle: pen.bitStyle ?? null,
			bitBandHeight: pen.bitBandHeight ?? null,
			limits: resolveLimits(pen, tag, isBit)
		};
	});
}

/**
 * グループの全ペンが bit か（説明文の言い方を替える）。bit の縦軸は 0〜1（小数桁は指定しない）で、値は
 * False / True で出る（部品が書く。表し方は表示グループのペンの属性で段差か帯か選べる）。
 */
export function isAllBitTrend(pens: readonly Pick<TrendPenInfo, 'isBit'>[]): boolean {
	return pens.length > 0 && pens.every((pen) => pen.isBit);
}

/** 凡例の文字（名前 + 単位。Q7: 単位は凡例に）。 */
export function penLegendLabel(pen: Pick<TrendPenInfo, 'name' | 'unit'>): string {
	return pen.unit ? `${pen.name}（${pen.unit}）` : pen.name;
}

/** 表示範囲の端の時刻（月/日 時:分:秒、端末のロケール）。 */
function trendTimeLabel(epochMs: number): string {
	const at = new Date(epochMs);
	if (Number.isNaN(at.getTime())) return String(epochMs);
	return at.toLocaleString(undefined, {
		month: 'numeric',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});
}

function formatNumber(n: number, decimals: number | null): string {
	return decimals === null ? String(n) : formatValue(n, decimals);
}

/** 描画データのうち表示範囲に描かれる値の最小と最大（ペン 1 本）。値が無ければ `null`。 */
function visibleExtent(
	data: TrendData | null,
	view: TimeRange | null,
	penId: string | undefined
): { min: number; max: number } | null {
	if (data === null || view === null || penId === undefined) return null;
	const series = drawableSeries(data, view).series[penId];
	let min = Infinity;
	let max = -Infinity;
	for (const b of series?.bins ?? []) {
		for (const v of [b.min, b.max]) {
			if (v === null || !Number.isFinite(v)) continue;
			if (v < min) min = v;
			if (v > max) max = v;
		}
	}
	return Number.isFinite(min) ? { min, max } : null;
}

/**
 * 画面外の説明文（純関数、D-2 の `meterDescription` と同じ役目）。図は `role="img"`
 * で中が読まれないので、ペン・単位・窓（追従を止めていれば表示中の期間）・表示範囲に描かれる
 * 値の範囲を文で伝える。値は描画データ（`hybridData` の結果）を表示範囲で絞って読む。
 */
export function trendDescription(input: {
	/** 時間窓（表示範囲がまだ無いときの言い方に使う）。 */
	windowSec: number;
	/** 表示範囲（`null` = まだ無い）。 */
	view: TimeRange | null;
	following: boolean;
	pens: readonly TrendPenInfo[];
	/** ペンの並びどおりの部品のペンの ID（`trendPenId`）。 */
	penIds: readonly string[];
	data: TrendData | null;
}): string {
	const { view, pens } = input;
	const parts: string[] = [];
	const penText = pens.map(penLegendLabel).join('、');
	const bitText = (v: number): string => (v === 0 ? 'False' : v === 1 ? 'True' : String(v));
	if (view === null || input.following) {
		const sec = view === null ? input.windowSec : Math.round((view.endMs - view.startMs) / 1000);
		parts.push(`直近 ${trendWindowLabel(sec)}のトレンド。ペン: ${penText}。`);
	} else {
		parts.push(
			`${trendTimeLabel(view.startMs)}〜${trendTimeLabel(view.endMs)} のトレンド（追従を止めています）。ペン: ${penText}。`
		);
	}
	const extents = pens.map((_, i) => visibleExtent(input.data, view, input.penIds[i]));
	const found = extents.filter((e): e is { min: number; max: number } => e !== null);
	if (found.length === 0) {
		parts.push('表示できる値はまだありません。');
	} else if (isAllBitTrend(pens)) {
		// 全ペンが bit: 縦軸は False と True。
		const lo = bitText(Math.min(...found.map((e) => e.min)));
		const hi = bitText(Math.max(...found.map((e) => e.max)));
		parts.push(`縦軸は False と True で、表示中の値は ${lo === hi ? lo : `${lo}〜${hi}`}。`);
	} else {
		const ranges = pens.map((pen, i) => {
			const extent = extents[i];
			if (extent === null) return `${pen.name} 値なし`;
			const lo = pen.isBit ? bitText(extent.min) : formatNumber(extent.min, pen.decimals);
			const hi = pen.isBit ? bitText(extent.max) : formatNumber(extent.max, pen.decimals);
			return `${pen.name} ${lo === hi ? lo : `${lo}〜${hi}`}`;
		});
		parts.push(`縦軸はペンごとの自動スケール。表示中の値の範囲: ${ranges.join('、')}。`);
		const bits = pens.filter((p) => p.isBit);
		if (bits.length > 0) {
			parts.push(`bit のペン（${bits.map((p) => p.name).join('、')}）は False か True。`);
		}
	}
	return parts.join('');
}

// --- 注記 --------------------------------------------------------------------

/** 履歴（表示範囲のうち過去の部分）の取得の状態。 */
export type TrendHistoryState = 'idle' | 'loading' | 'ready' | 'unavailable';

/** シミュレーション接続のペンに添える一文（D-3b の指示どおり）。 */
export const SIMULATION_NOTE =
	'シミュレーション接続の値は記録されないため、再読み込みで履歴は消えます';

export const HISTORY_UNAVAILABLE_NOTE =
	'直近の履歴を読み込めませんでした。表示を開いてからの値だけを描いています。';

/**
 * パネルに添える注記（純関数）。出す順に並べる。
 *
 * - 履歴を読めなかった（`unavailable`・失敗・時間切れ）: 現在値だけで描いていると言う
 *   （「履歴が 0 件」に潰さない）。
 * - 履歴に居ないタグ（消された等）: 名前を挙げる（1 本の誤りで全体を止めない）。
 * - シミュレーション接続のペン: 値が記録されないこと。
 */
export function trendNotices(input: {
	historyState: TrendHistoryState;
	unknownNames: readonly string[];
	simulationNames: readonly string[];
}): string[] {
	const notices: string[] = [];
	if (input.historyState === 'unavailable') notices.push(HISTORY_UNAVAILABLE_NOTE);
	if (input.unknownNames.length > 0) {
		notices.push(
			`次のタグは登録が見つからないため履歴がありません: ${input.unknownNames.join('、')}`
		);
	}
	if (input.simulationNames.length > 0) {
		notices.push(`${SIMULATION_NOTE}（${input.simulationNames.join('、')}）。`);
	}
	return notices;
}
