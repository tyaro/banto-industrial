/**
 * ハイブリッドの直近側: ペンごとのリングバッファ（設計書 §8）。
 *
 * - サンプルは `{ tMs, value }`。`value: null` は品質が悪い・現在値が無い時刻で、線を切る欠測。
 *   `liveFrom` は動かさない（欠測もバッファが知っている事実で、その時刻を履歴に戻さない）。
 * - `liveFrom` は「いまバッファが途切れなく持っている範囲の始まり」。購読を始めた後の最初の値の
 *   時刻、途切れた（`liveBreak`）後は、その後の最初の値の時刻。途切れより前のサンプルは捨てる
 *   （その範囲は履歴で描く）。始まりを待っている間（`liveFrom` が null）の `null` のサンプルは
 *   捨てる（何も知らない時刻を欠測として描かず、履歴に任せる）。
 * - 時刻が前のサンプル以下のもの（重複・巻き戻り）は捨てる。
 * - 長さは利用側が渡す保持時間 `retainMs`（設計では追従の窓の 2 倍か 30 分の長い方。
 *   `liveRetainMs`）。最新のサンプルの時刻から測る。
 *
 * **追い出しはまとめて行う。** 書き足すたびに保持時間を超えた分を 1 つずつ追い出すと、
 * `liveFrom` が毎回少しずつ進み、境界の区間の読み直し（`hybrid.ts` の `boundaryPlan`）が
 * サンプルのたびに走る。そこで、持っている長さが保持時間の `1 + evictSlack` 倍
 * （既定 1.1 倍）を超えたときだけ、保持時間まで一度に追い出す。保持 30 分なら `liveFrom` が
 * 進むのは約 3 分に 1 回になる。追い出した後の先頭の `null` のサンプルも捨て、`liveFrom` は
 * 残った最初の値の時刻にする（値が 1 つも残らなければ始まりを待つ状態に戻る）。
 *
 * 状態は不変（関数は新しいオブジェクトを返し、変わらなければ同じオブジェクトを返す。
 * `liveBreak` は常に空の `EMPTY_LIVE_PEN` を返す）。
 * Svelte の `$state.raw` にそのまま載せられる。
 */

/** 現在値 1 つ。`value: null` は欠測（品質が悪い・現在値なし）。 */
export interface LiveSample {
	tMs: number;
	value: number | null;
}

/** ペン 1 本のリングバッファ。`samples` は `tMs` の昇順で、すべて `liveFrom` 以後。 */
export interface LivePen {
	/** 途切れなく持っている範囲の始まり。始まりを待っている間は null。 */
	liveFrom: number | null;
	samples: readonly LiveSample[];
}

/** ペン id → リングバッファ。 */
export type LiveBuffers = Readonly<Record<string, LivePen | undefined>>;

export interface LiveOptions {
	/** 保持時間（ミリ秒、正の有限）。最新のサンプルから測る。 */
	retainMs: number;
	/** 追い出しを始めるまでの余裕の割合（既定 0.1）。0 なら毎回追い出す。 */
	evictSlack?: number;
}

/** 既定の保持時間の下限（30 分）。 */
export const LIVE_MIN_RETAIN_MS = 30 * 60_000;
/** 既定の追い出しの余裕の割合。 */
export const LIVE_EVICT_SLACK = 0.1;

/** 始まりを待っている空のバッファ。 */
export const EMPTY_LIVE_PEN: LivePen = Object.freeze({ liveFrom: null, samples: [] });

const fin = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

/** 設計書 §8 の保持時間: 追従の窓の 2 倍か 30 分の長い方。 */
export function liveRetainMs(followWindowMs: number): number {
	return fin(followWindowMs) && followWindowMs > 0
		? Math.max(2 * followWindowMs, LIVE_MIN_RETAIN_MS)
		: LIVE_MIN_RETAIN_MS;
}

/** 先頭の null を落として `liveFrom` を合わせる。値が無ければ空。 */
function normalize(samples: readonly LiveSample[]): LivePen {
	const i = samples.findIndex((s) => s.value !== null);
	if (i < 0) return EMPTY_LIVE_PEN;
	const rest = i === 0 ? samples : samples.slice(i);
	return { liveFrom: rest[0].tMs, samples: rest };
}

/**
 * サンプルを 1 つ書き足す。時刻が非有限・前以下のもの、待っている間の null は捨てる
 * （そのときは同じオブジェクトを返す）。値の非有限は null（欠測）として扱う。
 */
export function liveAppend(
	pen: LivePen | undefined,
	sample: LiveSample,
	options: LiveOptions
): LivePen {
	const cur = pen ?? EMPTY_LIVE_PEN;
	if (!fin(sample.tMs)) return cur;
	const value = fin(sample.value) ? sample.value : null;
	const last = cur.samples[cur.samples.length - 1];
	if (last && sample.tMs <= last.tMs) return cur;
	if (cur.liveFrom === null) {
		if (value === null) return cur;
		return { liveFrom: sample.tMs, samples: [{ tMs: sample.tMs, value }] };
	}
	const samples = [...cur.samples, { tMs: sample.tMs, value }];
	const retainMs = options.retainMs;
	if (!(fin(retainMs) && retainMs > 0)) return { liveFrom: cur.liveFrom, samples };
	const slack =
		fin(options.evictSlack) && options.evictSlack >= 0 ? options.evictSlack : LIVE_EVICT_SLACK;
	if (sample.tMs - samples[0].tMs <= retainMs * (1 + slack)) {
		return { liveFrom: cur.liveFrom, samples };
	}
	const cutoff = sample.tMs - retainMs;
	const keep = samples.findIndex((s) => s.tMs >= cutoff);
	return normalize(samples.slice(keep));
}

/**
 * 購読が途切れたことを知らせる。持っていたサンプルを捨て、次の値を待つ
 * （`liveFrom` は次の値の時刻に進む）。既に空なら同じオブジェクトを返す。
 */
export function liveBreak(_pen?: LivePen): LivePen {
	return EMPTY_LIVE_PEN;
}

/** 複数のペンへ書き足す（同じ時刻の現在値の 1 回分など）。何も変わらなければ同じオブジェクト。 */
export function liveAppendAll(
	buffers: LiveBuffers,
	samples: Readonly<Record<string, LiveSample | undefined>>,
	options: LiveOptions
): LiveBuffers {
	let out: Record<string, LivePen | undefined> | null = null;
	for (const [id, s] of Object.entries(samples)) {
		if (!s) continue;
		const prev = buffers[id];
		const next = liveAppend(prev, s, options);
		if (next === (prev ?? EMPTY_LIVE_PEN)) continue;
		out ??= { ...buffers };
		out[id] = next;
	}
	return out ?? buffers;
}

/** 途切れを知らせる（`penIds` を省くと全部）。何も変わらなければ同じオブジェクト。 */
export function liveBreakAll(buffers: LiveBuffers, penIds?: readonly string[]): LiveBuffers {
	let out: Record<string, LivePen | undefined> | null = null;
	for (const id of penIds ?? Object.keys(buffers)) {
		const prev = buffers[id];
		if (!prev || prev === EMPTY_LIVE_PEN) continue;
		out ??= { ...buffers };
		out[id] = liveBreak(prev);
	}
	return out ?? buffers;
}
