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
 * - `dead`: 消えたと確かめたタグの id。操作の列には入れない。表示から外し、利用者の操作で
 *   PUT するついでにだけ保存の一覧から落とす（古いタグ一覧を根拠に、自動で消すための PUT は
 *   しない: P2-2）。上限（`WATCH_LIMIT`）は `dead` を除いた件数で数える。
 * - `known`: ページの全件のタグ一覧の id（`setKnownLiveIds`）。**「生きている」の根拠にだけ
 *   使い、「消えた」の根拠にはしない**（PR #1282 の 4 回目のレビュー P2。一覧に無いことは、
 *   一覧が古い・読み直しが保留中なだけかもしれない）。
 *
 * 消えたかの判定（PR #1282 の 4 回目のレビュー P2、2026-11-22 オーナー決定）: タグ ID は再利用
 * されない（`crates/banto-tags/migrations/0003_tags.sql` の `INTEGER PRIMARY KEY AUTOINCREMENT`）
 * ので、ID ごとの `GET /api/tags/{id}` が 404 なら、その ID のタグは消えたと確定してよい
 * （`checkTagIds`、ページが渡す）。以前の「一覧の最大の ID より小さく一覧に無い」の判定は、
 * 最大の ID を消したときに確かめられず、見えない削除済みの ID が 100 件の枠を占めていた。
 * - 照会するのは、`known` にも `dead` にも無い id だけ。照会中の id は重ねて照会しない（周回と
 *   表示の確かめが同じ照会を待つ）。`gone` は `dead` に入れ、二度と照会しない。`exists` は
 *   その照会を待った周回の中でだけ生きているとし、覚えない（次に `known` に無ければまた照会
 *   してよい）。それ以外の失敗は「不明」で、`dead` に入れず上限にも数える（確かめられない物を
 *   勝手に消さない）。窓は不明の id を「確かめられないタグ」の行として出し、外せるようにする。
 * - 照会の途中で dispose されたら結果を当てない。
 * - 表示（`list`）= `base`（無ければ写し）から `dead` を除き、`ops` を当てたもの。
 *
 * 周回（1 本だけ）:
 * 1. その時点の操作の数 `n` を控え、`provider.get` で読む（provider は同じキーの書き込みを
 *    待ってから読む）。
 * 2. サーバーの値と、先頭 `n` 件の操作で足す id のうち、`known` にも `dead` にも無いものを照会し
 *    （上の「消えたかの判定」）、`gone` を `dead` に足す。**上限を当てる前に**削除を確かめる
 *    （最新の GET で初めて見えた削除済みの id が、その回の上限の判定で枠を占めないように）。
 *    読むだけの周回（`n === 0`）は、`known` がまだ無ければ照会しない（後の `setKnownLiveIds` で
 *    確かめる）。
 * 3. サーバーの値（壊れていれば空）から `dead` を除き、先頭 `n` 件の操作を当てる。`n === 0`
 *    なら当てず書かない（`base` = サーバーの値）。
 * 4. 結果がサーバーの値と違う（壊れた値に操作で中身ができた場合も）なら `provider.set` で書く。
 * 5. 成功したら `base` を結果にし、先頭 `n` 件の操作を捨て、写しを書く。周回の間に来た操作が
 *    残っていれば、すぐ次の周回。サーバーの値に当てて上限で落ちた件数は `onChange` で渡す
 *    （上限の最終の判定はここだけ。`apply` の手元の判定は仮で、知らせない。操作は成功の
 *    ときだけ捨てるので、同じ操作の上限超えを二度知らせない）。
 * 6. 失敗したら操作は残す。`unauthorized`・`forbidden`、または持ち主が替わったら `blocked` で
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
 *   `createSavedSetting` の失敗の再送）を提案した。入ったら手順 4 を条件付きにし、合わなければ
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
	withoutDeadInOp,
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
	/**
	 * 周回の中で、サーバーの値に当てて上限で落ちた件数（上限の最終の判定。`apply` の戻り値の
	 * `overLimit` は仮）。ふつうは 0。
	 */
	overLimit: number;
}

/** `GET /api/tags/{id}` の結果。404 は `gone`、200 は `exists`。 */
export type TagPresence = 'exists' | 'gone';

export interface WatchListStore {
	/** 最初の描画に使う写し。無い・壊れていれば空。 */
	readonly initial: number[];
	/**
	 * 今の一覧（表示に使う。`dead` を除く）。`known` に無い id（照会の失敗・照会の前・タグ一覧の
	 * 読み直しの保留中）も含む。窓はそれを「確かめられないタグ」の行として出す。
	 */
	readonly list: number[];
	readonly status: WatchSaveStatus;
	/**
	 * 操作を今の一覧に当て、保存の周回を始める。戻り値の `list` は当てた後の `list`。戻り値の
	 * `overLimit` は手元の仮の判定で、操作は捨てない（最終の判定は周回の `onChange`）。
	 */
	apply(op: WatchOp): AddResult;
	/**
	 * 生きていると分かっている id（ページの全件のタグ一覧）。照会を省くためだけに使い、ここに
	 * 無いことを「消えた」の根拠にしない。渡すと、一覧のうちここにも `dead` にも無い id を
	 * 照会する（表示の確かめ）。これだけでは保存しない。
	 */
	setKnownLiveIds(ids: Iterable<number>): void;
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
	/**
	 * id ごとの在否の照会（`GET /api/tags/{id}`、並列の数はページ側で絞る）。結果に入れた id
	 * だけが確定（`gone` は消えた、`exists` は生きている）で、入れない id は不明（照会の失敗）。
	 * reject は全件不明として扱う。無ければ照会しない（全部不明）。
	 */
	checkTagIds?: (ids: readonly number[]) => Promise<Map<number, TagPresence>>;
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
	let ops: WatchOp[] = [];
	const dead = new Set<number>();
	/** 生きていると分かっている id（`setKnownLiveIds`）。`null` はまだ渡されていない。 */
	let known: Set<number> | null = null;
	/** 照会中の id と、その結果（不明は `undefined`）。同じ id の照会を重ねない。 */
	const inflight = new Map<number, Promise<TagPresence | undefined>>();
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
		for (const op of ops) next = applyWatchOp(next, withoutDeadInOp(op, dead)).list;
		return next;
	}

	function notify(overLimit = 0): void {
		if (disposed) return;
		list = computeList();
		deps.onChange?.({ overLimit });
	}

	/**
	 * `ids` のうち `known` にも `dead` にも無いものの在否を確かめ、`gone` を `dead` に足す
	 * （冒頭「消えたかの判定」）。照会中の id は新しく照会せず、その結果を待つ。戻り値は、この
	 * 呼び出しで `dead` が増えたか。dispose の後は結果を当てない。
	 */
	async function verify(ids: Iterable<number>): Promise<boolean> {
		const check = deps.checkTagIds;
		if (check === undefined) return false;
		const targets = [...new Set(ids)].filter((id) => !dead.has(id) && !known?.has(id));
		const fresh = targets.filter((id) => !inflight.has(id));
		if (fresh.length > 0) {
			const batch = Promise.resolve()
				.then(() => check(fresh))
				.catch(() => new Map<number, TagPresence>());
			for (const id of fresh) {
				const one = batch.then((result) => result.get(id));
				inflight.set(id, one);
				void one.finally(() => {
					if (inflight.get(id) === one) inflight.delete(id);
				});
			}
		}
		const results = await Promise.all(
			targets.map(async (id) => [id, await inflight.get(id)] as const)
		);
		if (disposed) return false;
		let grew = false;
		for (const [id, presence] of results) {
			if (presence === 'gone' && !dead.has(id)) {
				dead.add(id);
				grew = true;
			}
		}
		return grew;
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
		// 上限を当てる前に、最新の GET の id（と操作で足す id）の削除を確かめる（冒頭の手順 2）。
		if (n > 0 || known !== null) {
			const candidates = [...server];
			for (const op of pending) if (op.kind === 'add') candidates.push(...op.ids);
			await verify(candidates);
			if (disposed) return false;
			if (ownerChanged()) return fail(null, 'read');
		}
		let next = server;
		let overLimit = 0;
		if (n > 0) {
			let applied = withoutIds(server, dead);
			for (const op of pending) {
				const result = applyWatchOp(applied, withoutDeadInOp(op, dead));
				applied = result.list;
				overLimit += result.overLimit;
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
		apply(op) {
			if (disposed) return { list, added: 0, duplicates: 0, overLimit: 0 };
			// 手元の上限の判定は仮（操作は捨てない）。最終の判定は周回の中（冒頭の手順 5）。
			const result = applyWatchOp(list, withoutDeadInOp(op, dead));
			// 操作の無い間の失敗（読み直しの失敗）を、新しい操作の「保存できていません」にしない。
			if (ops.length === 0 && lastError !== 'auth') lastError = null;
			ops.push(op);
			notify();
			kick();
			return { ...result, list };
		},
		setKnownLiveIds(ids) {
			if (disposed) return;
			known = new Set(ids);
			// 表示の確かめ: 周回とは別に走ってよい（同じ id の照会は `verify` が重ねない）。
			void verify(list).then((grew) => {
				if (grew) notify();
			});
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
