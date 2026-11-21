/**
 * 見張っているタグの一覧を利用者ごとに保存する小さな口（#1254。純粋な部分は `watchList.ts`）。
 * 置き場は `dockLayoutStore.ts` と同じ二層:
 * - localStorage（`banto.watch.v1.<持ち主>`）: 写し。最初の描画に使う。書けなくても黙って続ける。
 *   持ち主は作った時点で決め、分からなければ写しを読まず書きもしない。作った後に持ち主が
 *   替わっていたら写しを書かない。写しを書くのはサーバーの基準が確定した後だけ。
 * - `UiSettingsProvider`（`hub.watch.v1`）: 本体。デスクトップ版は起動ごとに origin が変わり
 *   localStorage が消えるので、マウント後に `load()` で読む。送信は `createSavedSetting`
 *   （即時の形）に任せる。
 *
 * サーバーの基準が確定する前の操作で、サーバーの一覧を消さない（PR #1282 のレビュー）:
 * - 一覧は「基準 + 操作」で持つ。`load()` が「読めた」か「保存値が無い（壊れていた）」で終わる
 *   までは、利用者の操作（{@link WatchOp}）を溜め、表示（`list`）は写し（無ければ空）に溜めた
 *   操作を当てたもの。この間は送らない（端末の写しや空から作った一覧が、サーバーにある一覧を
 *   上書きしないように。`createSavedSetting` は読み込み中の `set()` で読みを捨てる
 *   （`'stale'`）ので、送らないことは読みを生かすためにも要る）。
 * - 確定したら、サーバーの基準に溜めた操作を当てた結果を一覧にし、基準と違えば保存する。
 *   確定した後の操作は、今の一覧に当てて即保存する。
 * - 消えた id の除去（`prune`）は、確定する前は何もしない（写しから消えた id を落としても、
 *   サーバーの一覧の別の id を守れない。確定した後にタグ一覧と照らして落とす）。
 * - 読みが失敗した（`'failed'`）ときは、溜めた操作を捨てず表示に当てたままにし、保存はしない
 *   （サーバーを上書きしない）。
 *
 * 確定するまでの読み直し（PR #1282 の再レビュー、2026-11-22）: 初回の読みが一度失敗しただけで、
 * 足した見張りが保存されないまま画面を離れられないように、口が自分で読み直す。
 * - 失敗したら {@link RETRY_BASE_MS}（2 秒）から倍々に、{@link RETRY_MAX_MS}（60 秒）を上限に
 *   待って読み直す（確定するまで何度でも）。`dispose()` で止める。ページが隠れている
 *   （`document.visibilityState` が `hidden`）間は読まず、見えたらすぐ読む。
 * - 確定する前に操作を受けたら、待ちを打ち切ってすぐ読み直す（`retryNow()` も同じ。
 *   config_changed など、通信が戻った合図で呼び出し側が呼ぶ）。
 * - 確定する前に操作が溜まっている間は `unsaved` が立つ（窓に「保存待ち」を出す）。
 *   状態（`list`・`confirmed`・`unsaved`）が変わったら `onChange` で知らせる。
 * - それでも確定する前にページを離れた操作は失われる（サーバーを上書きしない方を取る）。
 */
import {
	createSavedSetting,
	getSessionController,
	type UiSettingsProvider
} from '@banto/admin-core';
import { getUiSettings } from './banto/setup';
import {
	applyWatchOp,
	applyWatchOps,
	parseWatchList,
	sameWatchList,
	serializeWatchList,
	watchListKeys,
	type AddResult,
	type WatchOp
} from './watchList';

/** 読みに失敗したときの最初の待ち（倍々に延ばす）。 */
export const RETRY_BASE_MS = 2_000;
/** 読みの待ちの上限。 */
export const RETRY_MAX_MS = 60_000;

export interface WatchListStore {
	/** 最初の描画に使う写し。無い・壊れていれば空。 */
	readonly initial: number[];
	/** 今の一覧（表示に使う）。確定する前は写しに溜めた操作を当てたもの。 */
	readonly list: number[];
	/** サーバーの基準が確定したか。 */
	readonly confirmed: boolean;
	/** 確定する前に操作が溜まっている（まだ保存していない）。窓の「保存待ち」に使う。 */
	readonly unsaved: boolean;
	/**
	 * 操作を今の一覧に当てる。確定した後は変わっていれば即保存し、確定する前は溜める。
	 * `prune` は確定する前は何もしない。戻り値の `list` は当てた後の `list`。
	 */
	apply(op: WatchOp): AddResult;
	/**
	 * サーバーの保存値を読み、基準を確定する。確定したら `true`（`list` が変わっていることが
	 * ある）。失敗したら `false` で、溜めた操作はそのまま（もう一度呼べば読み直す）。
	 */
	load(): Promise<boolean>;
	/** 確定していなければ、再試行の待ちを打ち切ってすぐ読み直す（確定していれば何もしない）。 */
	retryNow(): void;
	/** ページを離れるときに呼ぶ（読み直しも止める）。 */
	dispose(): void;
}

export interface WatchListStoreDeps {
	provider?: UiSettingsProvider;
	storage?: () => Pick<Storage, 'getItem' | 'setItem'> | undefined;
	owner?: () => string | null;
	/** `list`・`confirmed`・`unsaved` が変わったとき（読みの完了・操作の後）に呼ぶ。 */
	onChange?: () => void;
	/** ページが見えているか（既定は `document.visibilityState`）。テスト用。 */
	visible?: () => boolean;
	/** 見えるようになったら呼ぶ関数を登録し、外す関数を返す。テスト用。 */
	onVisible?: (listener: () => void) => () => void;
}

function defaultVisible(): boolean {
	return typeof document === 'undefined' || document.visibilityState !== 'hidden';
}

function defaultOnVisible(listener: () => void): () => void {
	if (typeof document === 'undefined') return () => {};
	const handler = (): void => {
		if (document.visibilityState !== 'hidden') listener();
	};
	document.addEventListener('visibilitychange', handler);
	return () => document.removeEventListener('visibilitychange', handler);
}

function defaultOwner(): string | null {
	try {
		const snapshot = getSessionController().snapshot;
		return snapshot.status === 'active' ? snapshot.owner : null;
	} catch {
		return null;
	}
}

function defaultStorage(): Pick<Storage, 'getItem' | 'setItem'> | undefined {
	try {
		return typeof localStorage === 'undefined' ? undefined : localStorage;
	} catch {
		return undefined;
	}
}

export function createWatchListStore(deps: WatchListStoreDeps = {}): WatchListStore {
	const currentOwner = deps.owner ?? defaultOwner;
	const owner = currentOwner();
	const keys = watchListKeys(owner);
	const storage = deps.storage ?? defaultStorage;
	const setting = createSavedSetting<number[]>({
		provider: deps.provider ?? getUiSettings(),
		key: keys.setting,
		parse: (raw) => parseWatchList(raw),
		serialize: (value) => serializeWatchList(value),
		fallback: () => [],
		mode: 'immediate'
	});

	function readLocal(): number[] {
		if (keys.local === undefined) return [];
		try {
			return parseWatchList(storage()?.getItem(keys.local)) ?? [];
		} catch {
			return [];
		}
	}

	function writeLocal(list: readonly number[]): void {
		if (keys.local === undefined || currentOwner() !== owner) return;
		try {
			storage()?.setItem(keys.local, serializeWatchList(list));
		} catch {
			// 写しは best-effort（本体はサーバー）。
		}
	}

	function persist(next: number[]): void {
		writeLocal(next);
		setting.set([...next]);
	}

	const initial = readLocal();
	let list: number[] = [...initial];
	/** サーバーの基準が確定したか。確定する前の操作は `pending` に溜める。 */
	let confirmed = false;
	let pending: WatchOp[] = [];
	let loading: Promise<boolean> | null = null;
	let disposed = false;
	/** 次の読み直しまでの待ち（失敗のたびに倍々）。 */
	let retryDelay = RETRY_BASE_MS;
	let retryTimer: ReturnType<typeof setTimeout> | null = null;
	/** 隠れている間に来た読み直しを、見えたときに行うための登録の解除。 */
	let stopWaitingVisible: (() => void) | null = null;
	const visible = deps.visible ?? defaultVisible;
	const onVisible = deps.onVisible ?? defaultOnVisible;

	function notify(): void {
		if (!disposed) deps.onChange?.();
	}

	function cancelRetry(): void {
		if (retryTimer !== null) clearTimeout(retryTimer);
		retryTimer = null;
		stopWaitingVisible?.();
		stopWaitingVisible = null;
	}

	/** 確定していなければ読む。隠れていれば、見えたときに読む。 */
	function retry(): void {
		cancelRetry();
		if (disposed || confirmed) return;
		if (!visible()) {
			stopWaitingVisible = onVisible(() => retry());
			return;
		}
		void startLoad();
	}

	function scheduleRetry(): void {
		cancelRetry();
		if (disposed || confirmed) return;
		const delay = retryDelay;
		retryDelay = Math.min(retryDelay * 2, RETRY_MAX_MS);
		retryTimer = setTimeout(() => {
			retryTimer = null;
			retry();
		}, delay);
	}

	function startLoad(): Promise<boolean> {
		if (disposed) return Promise.resolve(false);
		if (confirmed) return Promise.resolve(true);
		loading ??= loadOnce()
			.catch(() => false)
			.then((ok) => {
				loading = null;
				if (!ok) scheduleRetry();
				notify();
				return ok;
			});
		return loading;
	}

	async function loadOnce(): Promise<boolean> {
		const outcome = await setting.load();
		if (disposed || confirmed) return confirmed;
		// 'failed'（読めなかった）・'stale'（捨てた読み）は確定しない: 溜めた操作は残し、送らない。
		if (outcome !== 'loaded' && outcome !== 'missing' && outcome !== 'invalid') return false;
		const base = outcome === 'loaded' ? [...(setting.saved ?? [])] : [];
		const next = applyWatchOps(base, pending);
		pending = [];
		confirmed = true;
		list = next;
		if (sameWatchList(next, base) && outcome === 'loaded') {
			// サーバーの値そのまま: 送り返さず、写しだけ合わせる。
			writeLocal(next);
		} else if (outcome !== 'loaded' && next.length === 0) {
			// 保存値が無く、一覧も空: 送るものが無い。
			writeLocal(next);
		} else {
			persist(next);
		}
		return true;
	}

	return {
		initial,
		get list() {
			return list;
		},
		get confirmed() {
			return confirmed;
		},
		get unsaved() {
			return !confirmed && pending.length > 0;
		},
		apply(op) {
			const unchanged: AddResult = { list, added: 0, duplicates: 0, overLimit: 0 };
			if (disposed) return unchanged;
			if (!confirmed) {
				if (op.kind === 'prune') return unchanged;
				const result = applyWatchOp(list, op);
				pending.push(op);
				list = result.list;
				notify();
				// 溜めた操作を早く保存できるよう、読み直しの待ちを打ち切る（読み中ならそれを待つ）。
				if (loading === null) retry();
				return result;
			}
			const result = applyWatchOp(list, op);
			if (!sameWatchList(result.list, list)) {
				list = result.list;
				persist(list);
				notify();
			}
			return { ...result, list };
		},
		load() {
			return startLoad();
		},
		retryNow() {
			if (loading === null) retry();
		},
		dispose() {
			disposed = true;
			cancelRetry();
			setting.dispose();
		}
	};
}
