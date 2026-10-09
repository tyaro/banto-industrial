/**
 * 監視画面（`/monitor`、R1-D）の判断をまとめた純関数（`monitorLogic.test.ts` が
 * 表で固定する）。画面・パネル・ポーラーはここを呼ぶだけで、判断を書き写さない
 * （docs/implementation-checklist.md §5「判断は純関数に出して、状態の総当たりを
 * 表でテストする」）。
 *
 * ## 値の表示（2026-10-08 オーナー決定 Q2、docs/r1-plan.md の R1-D）
 *
 * - **null を 0 と区別する**（#414 段階2 の決定）。値が無いときは数値を出さず
 *   「—」にする。`0` は `0` として出す（`value ?? 0` のような潰し方をしない）。
 * - 品質 `invalid`（開始時に設定が不正で外したタグ）は「設定不正（収集対象外）」
 *   で、直す場所（`/tags`）へ案内する。通信エラーの `bad` と混ぜない
 *   （`collectAdmin.ts` の `qualityLabel`）。
 * - 現在値の表にキーが無いタグは「未収集（無効、または収集の再起動で反映）」。
 *   無効にしたタグ・収集を始めた後に足したタグがここに来る。
 * - **`bad` / `stale` では最後の値を出さない**。「—」と、最後に**使える値を
 *   受け取った**時刻（`lastGoodMs`）を小さく添える。一度も受け取っていなければ
 *   「受信した値はありません」。`ptimeMs`（読みに行った時刻。`bad` でも毎周期
 *   進む）は使わない（#531 レビュー: 切断中も時刻が進み、一度も読めていない
 *   タグも何か受け取ったように見えた）。
 *
 * - **bit のタグは `True` / `False`**（2026-10-09 オーナー決定、#551）。数値の 0 / 1
 *   では出さない。品質・しきい値の段の出し方は他のタグと同じ。タグ定義を読めて
 *   いない間は `dataType` が分からないので数値のまま出す。
 *
 * ## しきい値（Q5）
 *
 * **記録計の側のタグごとの設定**（#532。`tagThresholdsAdmin.ts`、タグ定義の属性では
 * ない）に今登録されているしきい値で、画面側で判定する。設定の無いタグは判定しない
 * （色も文字も出さない）。判定の向きと優先順位は
 * 収集のしきい値イベント（`crates/banto-collect/src/task.rs` の
 * `classify_threshold`）と同じ: 上側が下側より優先、HH/LL が H/L より優先、
 * 上側は `>=`、下側は `<=`。色だけで伝えない（パネルは文言も出す）。
 *
 * ## ポーリング周期（Q1）
 *
 * REST / Tauri のポーリングのまま（SSE にしない）。周期は、表示しているグループの
 * ペンのタグが属する収集グループの**最短の収集周期**を 500ms〜5s に丸めたもの。
 */
import { qualityLabel, type CurrentSampleView } from '../banto/collectAdmin';
import type { DisplayGroup, DisplayKind } from '../banto/displayGroupsAdmin';
import type { CollectionGroup, Tag } from '../banto/tagRegistryAdmin';
import {
	withThresholds,
	type TagThresholds as TagThresholdRow,
	type TagWithThresholds,
	type ThresholdFields
} from '../banto/tagThresholdsAdmin';

// --- グループの並びと選択 ----------------------------------------------------

/** 表示の並び（`sortOrder` 昇順、同じなら ID 昇順）。元の配列は変えない。 */
export function sortDisplayGroups(groups: readonly DisplayGroup[]): DisplayGroup[] {
	return [...groups].sort((a, b) => a.sortOrder - b.sortOrder || a.id - b.id);
}

/** URL の `?group=` の値を ID に読む。数字でなければ `null`。 */
export function parseGroupParam(raw: string | null): number | null {
	if (raw === null || !/^\d+$/.test(raw)) return null;
	const id = Number(raw);
	return Number.isSafeInteger(id) ? id : null;
}

/** 表示するグループの選び方の結果。 */
export interface GroupSelection {
	/** 表示するグループの ID（グループが 1 つも無ければ `null`）。 */
	id: number | null;
	/**
	 * URL で指定されたグループが見つからなかった（消された・ID の書き間違い）。
	 * 黙って別のグループを出すと、指定どおりに見えてしまうので画面が断る。
	 */
	missingRequested: number | null;
}

/**
 * 表示するグループを決める（純関数）。優先順位は URL（`?group=`）→ この端末で
 * 最後に見たグループ → 並びの先頭。どれも一覧に無いものは採らない。
 */
export function selectGroup(
	sorted: readonly DisplayGroup[],
	requested: number | null,
	remembered: number | null
): GroupSelection {
	const exists = (id: number | null): id is number =>
		id !== null && sorted.some((group) => group.id === id);
	if (exists(requested)) return { id: requested, missingRequested: null };
	const missingRequested = requested;
	if (exists(remembered)) return { id: remembered, missingRequested };
	return { id: sorted[0]?.id ?? null, missingRequested };
}

// --- 端末ごとの「最後に見たグループ」（localStorage） -------------------------

export const LAST_GROUP_STORAGE_KEY = 'chronogazer.monitor.lastGroup';

/** 端末に覚えた最後のグループ。ストレージが使えない・壊れていれば `null`。 */
export function loadLastGroup(storage: Pick<Storage, 'getItem'> | undefined): number | null {
	if (!storage) return null;
	try {
		return parseGroupParam(storage.getItem(LAST_GROUP_STORAGE_KEY));
	} catch {
		return null;
	}
}

/** 覚える（ベストエフォート。満杯・無効なストレージでも画面を止めない）。 */
export function saveLastGroup(storage: Pick<Storage, 'setItem'> | undefined, id: number): void {
	if (!storage) return;
	try {
		storage.setItem(LAST_GROUP_STORAGE_KEY, String(id));
	} catch {
		// 端末の便利機能なので、失敗しても表示は続ける。
	}
}

// --- ポーリング周期 ----------------------------------------------------------

export const MONITOR_POLL_MIN_MS = 500;
export const MONITOR_POLL_MAX_MS = 5000;
/** 周期を決められない（タグ・収集グループを読めていない、ペンが無い）ときの周期。 */
export const MONITOR_POLL_DEFAULT_MS = 1000;

/**
 * グループのペンの収集周期の最短を 500ms〜5s に丸める（純関数）。周期が 1 つも
 * 分からなければ [`MONITOR_POLL_DEFAULT_MS`]。
 */
export function pollPeriodMs(
	group: Pick<DisplayGroup, 'pens'>,
	tags: readonly Pick<Tag, 'id' | 'collectionGroupId'>[],
	collectionGroups: readonly Pick<CollectionGroup, 'id' | 'periodMs'>[]
): number {
	const periods: number[] = [];
	for (const pen of group.pens) {
		const tag = tags.find((t) => t.id === pen.tagId);
		if (!tag) continue;
		const cg = collectionGroups.find((g) => g.id === tag.collectionGroupId);
		if (cg && Number.isFinite(cg.periodMs) && cg.periodMs > 0) periods.push(cg.periodMs);
	}
	if (periods.length === 0) return MONITOR_POLL_DEFAULT_MS;
	return Math.min(MONITOR_POLL_MAX_MS, Math.max(MONITOR_POLL_MIN_MS, Math.min(...periods)));
}

// --- ペン 1 本の表示 ---------------------------------------------------------

/**
 * ペンの値の状態。`uncollected` = 現在値の表にキーが無い（無効なタグ・収集を
 * 始めた後に足したタグ）。品質の 4 つとは別の 5 つ目。
 */
export type PenState = 'good' | 'bad' | 'stale' | 'invalid' | 'uncollected';

/** しきい値の判定。`none` = 判定しない（しきい値が無い・値が無い）。 */
export type ThresholdLevel = 'HH' | 'H' | 'normal' | 'L' | 'LL' | 'none';

/** パネルに渡すペン 1 本の表示（パネルは判断せず、これを描くだけ）。 */
export interface PenView {
	tagId: number;
	/** タグ名（タグ情報が読めていなければ「タグ ID n」）。 */
	name: string;
	/** 大きく出す文字列。値が無ければ「—」（0 にしない）。 */
	display: string;
	/**
	 * 描画に使う数値（バーの長さ・計器の弧、D-2）。品質 good で値があるときだけ
	 * 数値で、それ以外（`bad` / `stale` / `invalid` / 未収集 / good で値なし）は
	 * `null`。`display` が「—」になるのと同じ条件で `null` になる（0 にしない）。
	 */
	value: number | null;
	unit: string | null;
	/** bit のタグ（`display` は True / False、バー・計器は 0〜1 が既定のレンジ）。 */
	isBit: boolean;
	state: PenState;
	/** 状態の文言（品質のラベル、または未収集の説明）。 */
	stateLabel: string;
	level: ThresholdLevel;
	/** しきい値の文言（`none` のときは `null`）。色だけで伝えないための文字。 */
	levelLabel: string | null;
	/**
	 * 最後に使える値を受け取った時刻（`lastGoodMs`、epoch ミリ秒）。`null` = 一度も
	 * 受け取っていない（`showLastReceived` のときだけ意味がある）。
	 */
	lastReceivedMs: number | null;
	/** 「最後に受け取った値」の行を出すか（`bad` / `stale` / 品質 good で値なし）。 */
	showLastReceived: boolean;
	/** 直す場所へのリンクを出すか（`invalid` = 設定不正）。 */
	linkToTags: boolean;
	/** banto チャートの系列色の枠（1..8）。 */
	colorSlot: number;
}

/** 一度も使える値を受け取っていないときの文言（#531 レビュー）。 */
export const NEVER_RECEIVED_LABEL = '受信した値はありません';

/**
 * 「最後に受け取った値」の行の文言（純関数）。出さないときは `null`。時刻は
 * `lastGoodMs` だけから作る（`ptimeMs` は使わない）。
 */
export function lastReceivedText(
	view: Pick<PenView, 'lastReceivedMs' | 'showLastReceived'>,
	timeLabel: (epochMs: number) => string
): string | null {
	if (!view.showLastReceived) return null;
	return view.lastReceivedMs === null
		? NEVER_RECEIVED_LABEL
		: `最後に受け取った値: ${timeLabel(view.lastReceivedMs)}`;
}

/** 値が無いときの表示（0 と区別する）。 */
export const NO_VALUE = '—';

/**
 * bit のタグの表示（2026-10-09 オーナー決定、#551）。既定で 0 / 1 ではなく
 * `True` / `False` と出す。文言は既定の表記として持つ（日本語の「オン / オフ」
 * などへ切り替えたくなったら、その時に設定を足す）。
 */
export const BIT_TRUE_LABEL = 'True';
export const BIT_FALSE_LABEL = 'False';

/** タグが bit か（タグを読めていなければ `false` = 数値のまま出す）。 */
export function isBitTag(tag: { dataType?: string } | undefined): boolean {
	return tag?.dataType === 'bit';
}

/** bit の値の表示（純関数）。`0` = `False`、それ以外 = `True`。 */
export function formatBitValue(value: number): string {
	return value === 0 ? BIT_FALSE_LABEL : BIT_TRUE_LABEL;
}

export const UNCOLLECTED_LABEL = '未収集（無効、または収集の再起動で反映）';
/** 品質は `good` なのに値が無い（サーバーは返さない約束だが、来たら 0 にしない）。 */
export const GOOD_WITHOUT_VALUE_LABEL = '値なし';

/**
 * 数値の表示（純関数）。小数桁はタグの `decimals`（0..15 に丸める）。`-0` は
 * `0` と出す。
 */
export function formatValue(value: number, decimals: number): string {
	const digits = Number.isInteger(decimals) ? Math.min(15, Math.max(0, decimals)) : 0;
	const text = value.toFixed(digits);
	return Number(text) === 0 ? (0).toFixed(digits) : text;
}

type TagThresholds = ThresholdFields;

/**
 * しきい値の判定（純関数）。`classify_threshold`（収集のしきい値イベント）と同じ
 * 向き・優先順位。しきい値が 1 つも無ければ `none`、あって当たらなければ `normal`。
 */
export function thresholdLevel(
	value: number | null,
	tag: TagThresholds | undefined
): ThresholdLevel {
	if (value === null || !tag) return 'none';
	const { thresholdHh: hh, thresholdH: h, thresholdLl: ll, thresholdL: l } = tag;
	if (hh == null && h == null && ll == null && l == null) return 'none';
	if (hh != null && value >= hh) return 'HH';
	if (h != null && value >= h) return 'H';
	if (ll != null && value <= ll) return 'LL';
	if (l != null && value <= l) return 'L';
	return 'normal';
}

/** しきい値の文言（色だけで伝えないための文字）。 */
export function thresholdLevelLabel(level: ThresholdLevel): string | null {
	switch (level) {
		case 'HH':
			return 'HH 上上限以上';
		case 'H':
			return 'H 上限以上';
		case 'L':
			return 'L 下限以下';
		case 'LL':
			return 'LL 下下限以下';
		case 'normal':
			return '範囲内';
		case 'none':
			return null;
	}
}

/**
 * ペン 1 本の表示を作る（純関数）。
 *
 * @param sample 現在値の表の該当エントリ（キーが無ければ `undefined` = 未収集）。
 * @param tag タグ定義（読めていなければ `undefined`。値は出すが単位・小数桁・
 *   しきい値は使えない）。
 */
export function penView(
	pen: { tagId: number; colorSlot: number | null },
	index: number,
	sample: CurrentSampleView | undefined,
	tag:
		| (Pick<Tag, 'name' | 'unit' | 'decimals'> & Partial<Pick<Tag, 'dataType'>> & TagThresholds)
		| undefined
): PenView {
	const base = {
		tagId: pen.tagId,
		name: tag?.name ?? `タグ ID ${pen.tagId}`,
		unit: tag?.unit ? tag.unit : null,
		isBit: isBitTag(tag),
		colorSlot: pen.colorSlot ?? index + 1
	};
	if (sample === undefined) {
		return {
			...base,
			display: NO_VALUE,
			value: null,
			state: 'uncollected',
			stateLabel: UNCOLLECTED_LABEL,
			level: 'none',
			levelLabel: null,
			lastReceivedMs: null,
			showLastReceived: false,
			linkToTags: false
		};
	}
	switch (sample.quality) {
		case 'invalid':
			return {
				...base,
				display: NO_VALUE,
				value: null,
				state: 'invalid',
				stateLabel: qualityLabel('invalid'),
				level: 'none',
				levelLabel: null,
				lastReceivedMs: null,
				showLastReceived: false,
				linkToTags: true
			};
		case 'bad':
		case 'stale':
			// 最後の値は出さない（Q2）。時刻だけ添える。
			return {
				...base,
				display: NO_VALUE,
				value: null,
				state: sample.quality,
				stateLabel: qualityLabel(sample.quality),
				level: 'none',
				levelLabel: null,
				lastReceivedMs: sample.lastGoodMs,
				showLastReceived: true,
				linkToTags: false
			};
		case 'good': {
			if (sample.value === null) {
				return {
					...base,
					display: NO_VALUE,
					value: null,
					state: 'bad',
					stateLabel: GOOD_WITHOUT_VALUE_LABEL,
					level: 'none',
					levelLabel: null,
					lastReceivedMs: sample.lastGoodMs,
					showLastReceived: true,
					linkToTags: false
				};
			}
			const level = thresholdLevel(sample.value, tag);
			return {
				...base,
				display: base.isBit
					? formatBitValue(sample.value)
					: tag
						? formatValue(sample.value, tag.decimals)
						: String(sample.value),
				value: sample.value,
				state: 'good',
				stateLabel: qualityLabel('good'),
				level,
				levelLabel: thresholdLevelLabel(level),
				lastReceivedMs: null,
				showLastReceived: false,
				linkToTags: false
			};
		}
	}
}

/**
 * グループの全ペンの表示（純関数）。`values` が `null`（まだ読めていない・
 * 収集が動いていない）なら空配列 - 画面はその場合パネルではなく状態の説明を出す。
 */
export function groupPenViews(
	group: Pick<DisplayGroup, 'pens'>,
	values: Readonly<Record<string, CurrentSampleView>> | null,
	tags: readonly TagWithThresholds[]
): PenView[] {
	if (values === null) return [];
	return group.pens.map((pen, index) =>
		penView(
			pen,
			index,
			values[`tag:${pen.tagId}`],
			tags.find((tag) => tag.id === pen.tagId)
		)
	);
}

// --- 表示種別 ----------------------------------------------------------------

/**
 * 描ける種別（D-1 でデジタル、D-2 でバー・計器、D-3b でトレンド。§3.2 の 4 種が
 * そろった）。サーバーが知らない種別を返したとき（版の食い違い）だけ `false`。
 */
export function isKindRendered(kind: DisplayKind): boolean {
	return kind === 'digital' || kind === 'bar' || kind === 'gauge' || kind === 'trend';
}

export const KIND_NOT_READY_MESSAGE = 'この表示種別は準備中です';

// --- 画面全体の状態 ----------------------------------------------------------

/** 現在値の取得の状態（ポーラーの状態から導く）。 */
export type ValuesPhase =
	/** まだ一度も結果が無い。 */
	| 'loading'
	/** 収集が動いていない（`notRunning`）。 */
	| 'notRunning'
	/** 値がある（読めている）。 */
	| 'ready';

/** 現在値を取得できていない間に添える一文（「いつの表示か」を必ず出す）。 */
export function valuesStaleNote(
	lastOkAt: number | null,
	timeLabel: (ms: number) => string
): string {
	if (lastOkAt === null) return '現在値を取得できていません（まだ一度も取得できていません）。';
	return `現在値を取得できていません。下の表示は${timeLabel(lastOkAt)}に取得したもので、最新ではありません。`;
}

// --- タグ情報としきい値の読み込み（#532 のレビュー P2） ---------------------------

/** 1 つの読み込みの結果（成功なら値、失敗なら理由の文字列）。 */
export type LoadResult<T> = { ok: true; value: T } | { ok: false; error: string };

/**
 * 監視画面が持つタグ情報としきい値。**2 つは別の失敗の軸**で、片方が読めなくても
 * もう片方は使う（`Promise.all` で 1 つにまとめると、しきい値だけの失敗でタグの
 * 名前・単位・小数桁まで捨ててしまう）。
 */
export interface TagMetaState {
	tags: Tag[];
	collectionGroups: CollectionGroup[];
	/** 判定に使うしきい値。読めなかったときは空（= 判定しない）で、前の値を残さない。 */
	thresholds: TagThresholdRow[];
	tagsError: string | null;
	thresholdsError: string | null;
}

export const INITIAL_TAG_META: TagMetaState = {
	tags: [],
	collectionGroups: [],
	thresholds: [],
	tagsError: null,
	thresholdsError: null
};

/**
 * 読み込みの結果を状態に当てる（純関数）。
 *
 * - タグ情報（タグ + 収集グループ）: 読めたら置き換える。読めなかったら前に読めた
 *   ものを残す（名前・単位・小数桁は古くても無いより正しい）。
 * - しきい値: 読めたら置き換える。**読めなかったら空にする**（古いしきい値で色分け・
 *   判定を続けると、設定を変えた後も古い判定を正しいように見せてしまう）。
 */
export function applyTagMetaLoad(
	prev: TagMetaState,
	meta: LoadResult<{ tags: Tag[]; collectionGroups: CollectionGroup[] }>,
	thresholds: LoadResult<TagThresholdRow[]>
): TagMetaState {
	return {
		tags: meta.ok ? meta.value.tags : prev.tags,
		collectionGroups: meta.ok ? meta.value.collectionGroups : prev.collectionGroups,
		tagsError: meta.ok ? null : meta.error,
		thresholds: thresholds.ok ? thresholds.value : [],
		thresholdsError: thresholds.ok ? null : thresholds.error
	};
}

/** 判定・描画に使うタグ（しきい値を添えた形）。 */
export function tagsForDisplay(state: TagMetaState): TagWithThresholds[] {
	return withThresholds(state.tags, state.thresholds);
}

/**
 * 読めなかったときの注記（純関数）。出す順に並べる。タグ情報は、まだ一度も読めて
 * いなければ従来の文言、前に読めたものを使っているならそう書く。しきい値は別の
 * 一文（色分け・判定をしていないことを言う）。
 */
export function tagMetaNotices(state: TagMetaState): string[] {
	const notices: string[] = [];
	if (state.tagsError !== null) {
		notices.push(
			state.tags.length === 0
				? `タグの情報を読み込めませんでした（${state.tagsError}）。単位・小数桁・しきい値なしで表示しています。`
				: `タグの情報を読み直せませんでした（${state.tagsError}）。前に読めたタグの情報で表示しています。`
		);
	}
	if (state.thresholdsError !== null) {
		notices.push(
			`しきい値を読み込めませんでした（${state.thresholdsError}）。色分け・しきい値の判定なしで表示しています。`
		);
	}
	return notices;
}
