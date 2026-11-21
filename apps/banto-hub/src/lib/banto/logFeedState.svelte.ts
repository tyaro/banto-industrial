/**
 * 下のログのパネルの状態（#1253）。取得と保持の判断は純粋な `logFeed.ts`
 * （`LogFeedPoller`）にあり、ここは runes への写しと、回す条件の配線だけを持つ。
 *
 * - 回すのは「パネルが見えている」（呼び出し側が `setVisible` で渡す。隠した・閉じた・
 *   狭い表示で退いた・タブの組の裏・畳んだは偽）かつ「ブラウザのタブが表」
 *   （`document.visibilityState === 'visible'`）の間だけ。どちらかが偽になったら止め、
 *   戻ったら続きから読む。ページを離れたら `dispose`。
 * - 状態はページが持つ（パネルの部品は隠すと作り直されることがあるので、部品には
 *   持たせない。再び見えたときに行と続きの位置が残っているように）。
 * - 一時停止: 読むのは続けるが表示は止め、止めた後に届いた新しい行の件数だけを出す
 *   （再開で今の行に揃える）。初回から読み直した（409・400）・資格を断られた（401・403）ときは、止めていても表示の
 *   行を捨てる（前の範囲の行を残さない）。
 * - 消去: 表示の行だけを捨てる（記録は消さない。続きの位置は残す）。
 */
import { getAuthProvider } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';
import {
	classifyLogFeedResponse,
	LOG_FEED_PATH,
	LogFeedPoller,
	type LogFeedEntry,
	type LogFeedFetch,
	type LogFeedSnapshot,
	type LogFeedStatus,
	type LogSource
} from './logFeed';

function currentToken(): string | null {
	try {
		const auth = getAuthProvider() as { getToken?: () => string | null };
		return auth.getToken ? auth.getToken() : null;
	} catch {
		return null;
	}
}

/** 口を 1 回読む（画面のセッションのトークンと CSRF のヘッダーを付ける）。 */
export const fetchLogFeed: LogFeedFetch = async (cursor, signal) => {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;
	const url =
		cursor === null ? LOG_FEED_PATH : `${LOG_FEED_PATH}?cursor=${encodeURIComponent(cursor)}`;
	let response: Response;
	try {
		response = await fetch(url, { headers, signal });
	} catch (error) {
		if (signal.aborted) throw error;
		return { kind: 'failed', message: 'サーバーに接続できません' };
	}
	let body: unknown;
	try {
		body = await response.json();
	} catch {
		body = null;
	}
	return classifyLogFeedResponse(response.status, body);
};

export interface LogFeedState {
	/** 表示する行（一時停止中は止めた時点の行）。 */
	readonly rows: readonly LogFeedEntry[];
	readonly status: LogFeedStatus;
	readonly error: string | null;
	readonly sources: readonly LogSource[];
	/** 回しているか（パネルが見えていて、タブが表）。 */
	readonly active: boolean;
	readonly paused: boolean;
	/** 一時停止の後に届いた新しい行の件数。 */
	readonly pendingCount: number;
	/** パネルが見えているか（呼び出し側の判定）。 */
	setVisible(visible: boolean): void;
	pause(): void;
	resume(): void;
	clear(): void;
	dispose(): void;
}

export interface LogFeedStateOptions {
	fetch?: LogFeedFetch;
	intervalMs?: number;
}

export function createLogFeedState(options: LogFeedStateOptions = {}): LogFeedState {
	let live = $state.raw<LogFeedSnapshot>({
		rows: [],
		status: 'idle',
		error: null,
		sources: [],
		active: false,
		epoch: 0
	});
	let paused = $state(false);
	let frozen = $state.raw<readonly LogFeedEntry[]>([]);
	let frozenEpoch = 0;
	let panelVisible = false;
	let documentVisible =
		typeof document === 'undefined' ? true : document.visibilityState === 'visible';

	const poller = new LogFeedPoller({
		fetch: options.fetch ?? fetchLogFeed,
		intervalMs: options.intervalMs,
		onChange: (snapshot) => {
			live = snapshot;
			if (paused && snapshot.epoch !== frozenEpoch) {
				frozen = [];
				frozenEpoch = snapshot.epoch;
			}
		}
	});

	function update(): void {
		poller.setActive(panelVisible && documentVisible);
	}

	const onVisibilityChange = (): void => {
		documentVisible = document.visibilityState === 'visible';
		update();
	};
	if (typeof document !== 'undefined') {
		document.addEventListener('visibilitychange', onVisibilityChange);
	}

	const pendingCount = $derived.by(() => {
		if (!paused) return 0;
		const seen = new Set(frozen.map((row) => row.key));
		let count = 0;
		for (const row of live.rows) if (!seen.has(row.key)) count += 1;
		return count;
	});

	return {
		get rows() {
			return paused ? frozen : live.rows;
		},
		get status() {
			return live.status;
		},
		get error() {
			return live.error;
		},
		get sources() {
			return live.sources;
		},
		get active() {
			return live.active;
		},
		get paused() {
			return paused;
		},
		get pendingCount() {
			return pendingCount;
		},
		setVisible(visible) {
			panelVisible = visible;
			update();
		},
		pause() {
			if (paused) return;
			frozen = live.rows;
			frozenEpoch = live.epoch;
			paused = true;
		},
		resume() {
			paused = false;
			frozen = [];
		},
		clear() {
			poller.clear();
			frozen = [];
		},
		dispose() {
			poller.dispose();
			if (typeof document !== 'undefined') {
				document.removeEventListener('visibilitychange', onVisibilityChange);
			}
		}
	};
}
