/**
 * 見張っているタグの一覧を利用者ごとに保存する口（#1254。純粋な部分は `watchList.ts`）。
 *
 * PR #1282 のレビューで保存の周りの指摘が 3 回続いたので、継ぎはぎをやめて作り直した
 * （2026-11-22 オーナー決定）。`createSavedSetting` は使わず、`UiSettingsProvider` を直接使う
 * （`createSavedSetting` は PUT の失敗を再送せず、読みの成功の時点で操作を捨てていた: P2-1）。
 *
 * 正は「サーバーの値 + まだ確かめられていない操作」:
 * - `base`: 最後に確かめたサーバーの一覧（`null` は一度も読めていない）。
 * - `ops`: まだ確かめられていない利用者の操作（足す・外す・すべて外す）。**PUT が成功する
 *   まで捨てない**。
 * - `dead`: 消えたと確かめたタグの id（ページが `setDeadIds` で渡す。判定は
 *   `watchList.ts::confirmedDeadIds`）。操作の列には入れない。表示から外し、利用者の操作で
 *   PUT するついでにだけ保存の一覧から落とす（古いタグ一覧を根拠に、自動で消すための PUT は
 *   しない: P2-2）。上限（`WATCH_LIMIT`）は `dead` を除いた件数で数える。
 * - 表示（`list`）= `base`（無ければ写し）から `dead` を除き、`ops` を当てたもの。
 *
 * 周回（1 本だけ）:
 * 1. その時点の操作の数 `n` を控え、`provider.get` で読む（provider は同じキーの書き込みを
 *    待ってから読む）。
 * 2. サーバーの値（壊れていれば空）から `dead` を除き、先頭 `n` 件の操作を当てる。`n === 0`
 *    （読むだけの周回）なら当てず書かない（`base` = サーバーの値）。
 * 3. 結果がサーバーの値と違う（壊れた値に操作で中身ができた場合も）なら `provider.set` で書く。
 * 4. 成功したら `base` を結果にし、先頭 `n` 件の操作を捨て、写しを書く。周回の間に来た操作が
 *    残っていれば、すぐ次の周回。サーバーの値に当てて上限で落ちた件数（足した時点で知らせた
 *    分を除く）は `onChange` で渡す。
 * 5. 失敗したら操作は残す。`unauthorized`・`forbidden`、または持ち主が替わったら `blocked` で
 *    止める（自動で再送しない、写しも書かない）。それ以外は {@link RETRY_BASE_MS} から倍々に
 *    {@link RETRY_MAX_MS} を上限に待って再送する（ページが隠れている間は待つ）。新しい操作・
 *    `retryNow()`（config_changed・窓の「今すぐ再試行」）は待ちを打ち切ってすぐ周回する。
 *
 * 操作は当て直しても同じ結果になる（足すは重複を足さず、外すは無ければ何もしない）。PUT が
 * 届いて応答だけ失われても、次の周回の GET に入っているので PUT なしで確定する。別のタブが
 * 足した分も、毎回サーバーの値に当てるので消えない。
 *
 * 写し（localStorage `banto.watch.v1.<持ち主>`）: 最初の描画に使う。書くのは周回の成功の後だけ、
 * かつ持ち主が作った時点と同じときだけ。持ち主が分からなければ読まず書かない。デスクトップ版は
 * 起動ごとに origin が変わり localStorage が消えるので、本体はサーバー（`hub.watch.v1`）。
 *
 * 受け入れる残りの穴（制約）:
 * - GET と PUT の間に同じ利用者の別のタブが書くと、その書き込みを上書きし得る。ui-settings に
 *   条件付きの PUT が無いため。tyaro/banto#786（ui-settings の条件付き PUT と、
 *   `createSavedSetting` の失敗の再送）を提案した。入ったら手順 3 を条件付きにし、合わなければ
 *   1 からやり直す。
 * - 保存が成功する前にページを離れた操作は失われる（窓に「保存中」「保存できていません」を
 *   出して知らせる）。
 * - `clear` の再送は、間に別のタブが足した分も消す。
 */
import { getSessionController, type UiSettingsProvider } from '@banto/admin-core';
import { getUiSettings } from './banto/setup';
import {
	applyWatchOp,
	parseWatchList,
	sameWatchList,
	serializeWatchList,
	watchListKeys,
	withoutIds,
	type AddResult,
	type WatchOp
} from './watchList';

/** 周回に失敗したときの最初の待ち（倍々に延ばす）。 */
export const RETRY_BASE_MS = 2_000;
/** 待ちの上限。 */
export const RETRY_MAX_MS = 60_000;

/**
 * 保存の状態（窓の表示に使う）。
 * - `loading`: まだ一度も読めておらず、操作も無い（表示しない。写しを表示する）。
 * - `saved`: 読めていて、操作が無い（表示しない）。
 * - `saving`: 操作があり、周回中か直後に走る（「保存中」）。
 * - `retrying`: 操作があり、直前の周回が失敗して再送を待っている（「保存できていません」）。
 * - `blocked`: ログインの失効・権限・持ち主の替わりで保存できない。自動の再送はしない。
 */
export type WatchSaveStatus = 'loading' | 'saved' | 'saving' | 'retrying' | 'blocked';

export interface WatchListChange {
	/** サーバーの値に当てて上限で落ちた件数（足した時点で知らせた分を除く）。ふつうは 0。 */
	overLimit: number;
}

export interface WatchListStore {
	/** 最初の描画に使う写し。無い・壊れていれば空。 */
	readonly initial: number[];
	/** 今の一覧（表示に使う）。 */
	readonly list: number[];
	readonly status: WatchSaveStatus;
	/**
	 * 消えたかを判定する対象の id（`dead` を除く前: サーバーの値（無ければ写し）と、操作で
	 * 足した id）。`dead` を除いた `list` で判定すると、除いた id が判定から外れて戻ってしまう。
	 */
	readonly deadCandidates: number[];
	/** 操作を今の一覧に当て、保存の周回を始める。戻り値の `list` は当てた後の `list`。 */
	apply(op: WatchOp): AddResult;
	/** 消えたと確かめた id（`confirmedDeadIds`）。表示から外す。これだけでは保存しない。 */
	setDeadIds(ids: Iterable<number>): void;
	/** 周回を始め、口が手すきになるまで待つ（最初の読み込みに使う）。 */
	load(): Promise<void>;
	/** 再送の待ちを打ち切ってすぐ周回する（`blocked` のときは何もしない）。 */
	retryNow(): void;
	/** ページを離れるときに呼ぶ（以後、周回の結果を当てず知らせない）。 */
	dispose(): void;
}

export interface WatchListStoreDeps {
	provider?: UiSettingsProvider;
	storage?: () => Pick<Storage, 'getItem' | 'setItem'> | undefined;
	owner?: () => string | null;
	/** `list`・`status` が変わったときに呼ぶ。 */
	onChange?: (change: WatchListChange) => void;
	/** ページが見えているか（既定は `document.visibilityState`）。テスト用。 */
	visible?: () => boolean;
	/** 見えるようになったら呼ぶ関数を登録し、外す関数を返す。テスト用。 */
	onVisible?: (listener: () => void) => () => void;
}

/** まだ確かめられていない操作。`shownOverLimit` は足した時点で知らせた上限超えの件数。 */
interface PendingOp {
	op: WatchOp;
	shownOverLimit: number;
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

/**
 * ログインの失効・権限の不足か。provider は `ProviderError`（`body.kind`）で reject する
 * （セッションが替わって送らなかった書き込みも `unauthorized`）。
 */
function isAuthError(error: unknown): boolean {
	if (typeof error !== 'object' || error === null) return false;
	const body = (error as { body?: { kind?: unknown } }).body;
	const kind = body?.kind ?? (error as { kind?: unknown }).kind;
	return kind === 'unauthorized' || kind === 'forbidden';
}

export function createWatchListStore(deps: WatchListStoreDeps = {}): WatchListStore {
	const currentOwner = deps.owner ?? defaultOwner;
	const owner = currentOwner();
	const keys = watchListKeys(owner);
	const storage = deps.storage ?? defaultStorage;
	const provider = deps.provider ?? getUiSettings();
	const visible = deps.visible ?? defaultVisible;
	const onVisible = deps.onVisible ?? defaultOnVisible;

	function readLocal(): number[] {
		if (keys.local === undefined) return [];
		try {
			return parseWatchList(storage()?.getItem(keys.local)) ?? [];
		} catch {
			return [];
		}
	}

	function writeLocal(next: readonly number[]): void {
		if (keys.local === undefined || currentOwner() !== owner) return;
		try {
			storage()?.setItem(keys.local, serializeWatchList(next));
		} catch {
			// 写しは best-effort（本体はサーバー）。
		}
	}

	const initial = readLocal();
	let base: number[] | null = null;
	let ops: PendingOp[] = [];
	let dead = new Set<number>();
	let list: number[] = [...initial];
	let lastError: null | 'read' | 'write' | 'auth' = null;
	let disposed = false;
	/** 走っている周回の列（1 本だけ）。 */
	let running: Promise<void> | null = null;
	/** 周回の途中で `kick` された（終わったらもう一度回る）。 */
	let again = false;
	let retryDelay = RETRY_BASE_MS;
	let retryTimer: ReturnType<typeof setTimeout> | null = null;
	let stopWaitingVisible: (() => void) | null = null;

	function computeList(): number[] {
		let next = withoutIds(base ?? initial, dead);
		for (const p of ops) next = applyWatchOp(next, p.op).list;
		return next;
	}

	function notify(overLimit = 0): void {
		if (disposed) return;
		list = computeList();
		deps.onChange?.({ overLimit });
	}

	function ownerChanged(): boolean {
		return currentOwner() !== owner;
	}

	function cancelRetry(): void {
		if (retryTimer !== null) clearTimeout(retryTimer);
		retryTimer = null;
		stopWaitingVisible?.();
		stopWaitingVisible = null;
	}

	function scheduleRetry(): void {
		cancelRetry();
		if (disposed) return;
		const delay = retryDelay;
		retryDelay = Math.min(retryDelay * 2, RETRY_MAX_MS);
		retryTimer = setTimeout(() => {
			retryTimer = null;
			kick();
		}, delay);
	}

	/** 待ちを打ち切って周回する。周回中なら、終わった後にもう一度回る。 */
	function kick(): void {
		cancelRetry();
		if (disposed || lastError === 'auth') return;
		if (running !== null) {
			again = true;
			return;
		}
		if (!visible()) {
			stopWaitingVisible = onVisible(() => kick());
			return;
		}
		running = loop().finally(() => {
			running = null;
		});
	}

	async function loop(): Promise<void> {
		for (;;) {
			again = false;
			const ok = await syncOnce();
			if (disposed) return;
			if (!ok) {
				if (lastError === 'auth') return;
				if (again) continue;
				// 読めていて操作も無い（config_changed の読み直しの失敗）なら、待って読み直さない。
				if (base !== null && ops.length === 0) return;
				scheduleRetry();
				return;
			}
			if (ops.length === 0 && !again) return;
		}
	}

	function fail(error: unknown, kind: 'read' | 'write'): false {
		if (disposed) return false;
		lastError = isAuthError(error) || ownerChanged() ? 'auth' : kind;
		notify();
		return false;
	}

	async function syncOnce(): Promise<boolean> {
		const n = ops.length;
		const pending = ops.slice(0, n);
		let raw: string | null;
		try {
			raw = await provider.get(keys.setting);
		} catch (error) {
			return fail(error, 'read');
		}
		if (disposed) return false;
		if (ownerChanged()) return fail(null, 'read');
		const parsed = parseWatchList(raw);
		const server = parsed ?? [];
		let next = server;
		let overLimit = 0;
		if (n > 0) {
			let applied = withoutIds(server, dead);
			for (const p of pending) {
				const result = applyWatchOp(applied, p.op);
				applied = result.list;
				overLimit += Math.max(0, result.overLimit - p.shownOverLimit);
			}
			next = applied;
			const broken = raw !== null && raw !== '' && parsed === undefined;
			if (!sameWatchList(next, server) || (broken && next.length > 0)) {
				try {
					await provider.set(keys.setting, serializeWatchList(next));
				} catch (error) {
					return fail(error, 'write');
				}
				if (disposed) return false;
			}
		}
		base = [...next];
		ops = ops.slice(n);
		lastError = null;
		retryDelay = RETRY_BASE_MS;
		writeLocal(next);
		notify(overLimit);
		return true;
	}

	return {
		initial,
		get list() {
			return list;
		},
		get status(): WatchSaveStatus {
			if (lastError === 'auth') return 'blocked';
			if (ops.length === 0) return base === null ? 'loading' : 'saved';
			return lastError === null ? 'saving' : 'retrying';
		},
		get deadCandidates() {
			const ids = new Set(base ?? initial);
			for (const p of ops) if (p.op.kind === 'add') for (const id of p.op.ids) ids.add(id);
			return [...ids];
		},
		apply(op) {
			if (disposed) return { list, added: 0, duplicates: 0, overLimit: 0 };
			const result = applyWatchOp(list, op);
			// 操作の無い間の失敗（読み直しの失敗）を、新しい操作の「保存できていません」にしない。
			if (ops.length === 0 && lastError !== 'auth') lastError = null;
			ops.push({ op, shownOverLimit: result.overLimit });
			notify();
			kick();
			return { ...result, list };
		},
		setDeadIds(ids) {
			const next = new Set(ids);
			if (next.size === dead.size && [...next].every((id) => dead.has(id))) return;
			dead = next;
			notify();
		},
		load() {
			kick();
			return running ?? Promise.resolve();
		},
		retryNow() {
			kick();
		},
		dispose() {
			disposed = true;
			cancelRetry();
		}
	};
}
