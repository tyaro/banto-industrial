/**
 * 下のログのパネルの取得と保持（#1253。docs/hub/banto-hub-ui-improvement-proposal.md
 * §下のログのパネル）。runes を使わない純粋な部分で、単体テストできる
 * （画面の状態への配線は `logFeedState.svelte.ts`、表は `LogPanel.svelte`）。
 *
 * 口は `GET /api/log-feed`（`apps/banto-hub/core/src/rest/log_feed.rs`）。
 *
 * - **進み具合と表示の順を分ける**: 続きの位置は口が返す不透明な `cursor` だけで持ち、
 *   表示は行の発生の時刻（`ts_ms`）の降順。後から届いた古い時刻の行は時刻の位置に
 *   差し込む（`audit_log` は保留から元の時刻のまま後で入る）。
 * - **行は `key` で持つ**。同じ `key` が届いたら置き換える（書き込みの暫定の `failed` が
 *   確定した版で届く。暫定のまま失敗と出し続けない）。
 * - 保持には上限（既定 1,000 行）を置き、超えたら古い時刻から捨てる。
 * - **409（`cursor_expired`）と 400 は、続きの位置と表示中の行を全部捨て、初回から
 *   読み直す**（Hub の再起動・ロールの変化でカーソルが使えなくなる。ロールが下がった
 *   ときに前の範囲の行を残さない）。401・403 はセッションの失効で、画面の既存の流儀
 *   （SSE → SessionController → ログイン画面）に任せ、ここでは回し続けない。
 * - 回すのは見えている間だけ（`setActive`）。止めても位置と行は残し、再び見えたら
 *   続きから読む。`more: true` はすぐ続けて読むが、続けて読む回数に上限を置く。
 * - ネットワークの失敗・5xx は間隔を延ばして（倍々、上限あり）再試行する。
 * - 飛行中の応答は、止めた・読み直した後に届いたら捨てる（世代の番号で照合。新しい
 *   状態を古い応答で巻き戻さない）。
 */

/** 接続の行（`collect_events` の PLC の接続・切断・復帰と、受け入れの切断）。 */
export interface ConnectionLogEntry {
	source: 'connection';
	/** 管理者未満への受け入れの切断の行には無い。 */
	id?: number;
	key: string;
	ts_ms: number;
	/** `plc_connected`・`plc_disconnected`・`plc_reconnected`・`ingest_disconnected`。 */
	kind: string;
	connection_id: number | null;
	connection_name: string | null;
	detail: string | null;
	transport: string | null;
	reason: string | null;
	/** 管理者だけ。 */
	audit_detail?: unknown;
}

/** 書き込みの監査の行（管理者だけ）。 */
export interface WriteLogEntry {
	source: 'write';
	id: number;
	key: string;
	ts_ms: number;
	change_seq: number;
	/** `write`・`rate_limit_tripped`。 */
	action: string;
	/** `ok`・`failed`・`uncertain`・`rejected`・`suppressed_disabled`・`suppressed_rate_limited`。 */
	result: string;
	tag_id: number;
	tag_name: string;
	value: number | null;
	value_text: string | null;
	detail: string | null;
	api_key_id: number;
	api_key_name: string;
	api_key_prefix: string | null;
	correlation_id: string | null;
	operator_claim: string | null;
}

/** 操作・認証の監査の行（管理者だけ）。 */
export interface OperationLogEntry {
	source: 'operation';
	id: number;
	key: string;
	ts_ms: number;
	/** `mcp`・`rest`・`grpc`。 */
	origin: string;
	actor_username: string | null;
	actor_role: string | null;
	action: string;
	resource: string;
	entity_id: string | null;
	/** `ok`・`denied`・`failed`。 */
	result: string;
	detail: unknown;
}

export type LogFeedEntry = ConnectionLogEntry | WriteLogEntry | OperationLogEntry;
export type LogSource = LogFeedEntry['source'];

/** 口の応答。 */
export interface LogFeedResponse {
	entries: LogFeedEntry[];
	cursor: string;
	more: boolean;
}

/** 1 回の取得の結果の分類。 */
export type LogFeedFetchOutcome =
	| { kind: 'ok'; response: LogFeedResponse }
	/** 409 `cursor_expired`・400: 位置と行を捨てて初回から。 */
	| { kind: 'reset' }
	/** 401・403: セッションの失効（既存の流儀に任せ、回し続けない）。 */
	| { kind: 'unauthorized' }
	/** ネットワーク・5xx・時間切れなど: 間隔を延ばして再試行。 */
	| { kind: 'failed'; message: string };

export const LOG_FEED_PATH = '/api/log-feed';

/** 409 の `current.code`（`log_feed.rs` の `CURSOR_EXPIRED_CODE`）。 */
export const CURSOR_EXPIRED_CODE = 'cursor_expired';

/** 口の応答の状態と本文を分類する（`fetch` の外に出して表でテストする）。 */
export function classifyLogFeedResponse(status: number, body: unknown): LogFeedFetchOutcome {
	if (status >= 200 && status < 300) {
		if (isLogFeedResponse(body)) return { kind: 'ok', response: body };
		return { kind: 'failed', message: '応答の形が違います' };
	}
	if (status === 401 || status === 403) return { kind: 'unauthorized' };
	if (status === 400) return { kind: 'reset' };
	if (status === 409) {
		const code = (body as { current?: { code?: unknown } } | null)?.current?.code;
		if (code === CURSOR_EXPIRED_CODE) return { kind: 'reset' };
		return { kind: 'failed', message: `${status}` };
	}
	return { kind: 'failed', message: `${status}` };
}

function isLogFeedResponse(value: unknown): value is LogFeedResponse {
	if (typeof value !== 'object' || value === null) return false;
	const v = value as Partial<LogFeedResponse>;
	return Array.isArray(v.entries) && typeof v.cursor === 'string' && typeof v.more === 'boolean';
}

/** 行の並び（発生の時刻の降順。同じ時刻は `key` の降順で決めて揺らさない）。 */
export function compareLogEntries(a: LogFeedEntry, b: LogFeedEntry): number {
	if (a.ts_ms !== b.ts_ms) return b.ts_ms - a.ts_ms;
	return a.key < b.key ? 1 : a.key > b.key ? -1 : 0;
}

/**
 * 受け取った行を今の行へ混ぜる: 同じ `key` は置き換え、時刻の位置に差し込み、上限を
 * 超えたら古い時刻から捨てる。元の配列は変えない。
 */
export function mergeLogEntries(
	current: readonly LogFeedEntry[],
	incoming: readonly LogFeedEntry[],
	maxRows: number
): LogFeedEntry[] {
	if (incoming.length === 0 && current.length <= maxRows) return [...current];
	const byKey = new Map<string, LogFeedEntry>();
	for (const row of current) byKey.set(row.key, row);
	for (const row of incoming) byKey.set(row.key, row);
	const merged = [...byKey.values()].sort(compareLogEntries);
	return merged.length > maxRows ? merged.slice(0, Math.max(0, maxRows)) : merged;
}

/** 取得の口の 1 回分（`cursor` が `null` なら初回）。`signal` で打ち切れること。 */
export type LogFeedFetch = (
	cursor: string | null,
	signal: AbortSignal
) => Promise<LogFeedFetchOutcome>;

/** 画面へ知らせる状態。 */
export type LogFeedStatus =
	/** まだ読んでいない・止めている。 */
	| 'idle'
	| 'ok'
	/** 失敗して再試行を待っている（`error` に文言）。 */
	| 'retrying'
	/** セッションが失効した（回さない）。 */
	| 'unauthorized';

export interface LogFeedSnapshot {
	/** 保持している行（表示の順）。 */
	rows: readonly LogFeedEntry[];
	status: LogFeedStatus;
	/** 最後の失敗の文言（成功したら `null`）。 */
	error: string | null;
	/** 届いた種類（トグルを届いた種類に合わせる。初回から読み直すと空に戻る）。 */
	sources: readonly LogSource[];
	/** 回しているか（見えている）。 */
	active: boolean;
	/** 初回から読み直した回数（増えたら、画面が控えた行も捨てる。一時停止中の表示など）。 */
	epoch: number;
}

export interface LogFeedTimers {
	setTimeout: (fn: () => void, ms: number) => unknown;
	clearTimeout: (handle: unknown) => void;
}

export interface LogFeedPollerOptions {
	fetch: LogFeedFetch;
	onChange: (snapshot: LogFeedSnapshot) => void;
	/** 読む間隔（既定 5 秒）。 */
	intervalMs?: number;
	/** 失敗の後の間隔の上限（既定 60 秒）。 */
	maxBackoffMs?: number;
	/** `more: true` で続けて読む回数の上限（既定 10。超えたら通常の間隔に戻る）。 */
	maxBurst?: number;
	/** 保持する行の上限（既定 1,000）。 */
	maxRows?: number;
	/** 1 回の取得の期限（既定 15 秒）。 */
	timeoutMs?: number;
	/** 時計（試験で差し替える）。 */
	timers?: LogFeedTimers;
}

const SOURCE_ORDER: readonly LogSource[] = ['connection', 'write', 'operation'];

/**
 * ログの取得を回す係。`setActive(true)` で回し始め（すぐ 1 回読む）、`setActive(false)` で
 * 止める（予約と飛行中の応答を捨てる。位置と行は残す）。`dispose` は最後の決定。
 */
export class LogFeedPoller {
	private cursor: string | null = null;
	private rows: LogFeedEntry[] = [];
	private sources = new Set<LogSource>();
	private status: LogFeedStatus = 'idle';
	private error: string | null = null;
	private active = false;
	private disposed = false;
	private generation = 0;
	private timer: unknown = null;
	private controller: AbortController | null = null;
	private inFlight = false;
	private failures = 0;
	private burst = 0;
	/**
	 * 続けて初回から読み直した回数。最初の 1 回はすぐ読み直し、2 回目からは通常の間隔を
	 * 置く（409・400 を返し続ける口に詰めて読みに行かない）。
	 */
	private resets = 0;
	private epoch = 0;
	private readonly intervalMs: number;
	private readonly maxBackoffMs: number;
	private readonly maxBurst: number;
	private readonly maxRows: number;
	private readonly timeoutMs: number;
	private readonly timers: LogFeedTimers;

	constructor(private readonly options: LogFeedPollerOptions) {
		this.intervalMs = options.intervalMs ?? 5000;
		this.maxBackoffMs = options.maxBackoffMs ?? 60_000;
		this.maxBurst = options.maxBurst ?? 10;
		this.maxRows = options.maxRows ?? 1000;
		this.timeoutMs = options.timeoutMs ?? 15_000;
		this.timers = options.timers ?? {
			setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
			clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>)
		};
	}

	snapshot(): LogFeedSnapshot {
		return {
			rows: this.rows,
			status: this.status,
			error: this.error,
			sources: SOURCE_ORDER.filter((s) => this.sources.has(s)),
			active: this.active,
			epoch: this.epoch
		};
	}

	/** 見えている間だけ回す。真になったらすぐ読み（続きから）、偽になったら止める。 */
	setActive(active: boolean): void {
		if (this.disposed || active === this.active) return;
		this.active = active;
		this.cancel();
		if (active) {
			// セッションの失効の後も、見せ直したら 1 回は試す（ログインし直した後など）。
			if (this.status === 'unauthorized') this.status = 'idle';
			this.failures = 0;
			this.burst = 0;
			this.schedule(0);
		}
		this.emit();
	}

	/** 表示中の行を消す（表示だけ。位置は残すので、以後に届いた行だけが出る）。 */
	clear(): void {
		if (this.disposed) return;
		this.rows = [];
		this.emit();
	}

	/** ページを離れるとき。予約・飛行中の応答を捨て、以後は何もしない。 */
	dispose(): void {
		this.disposed = true;
		this.active = false;
		this.cancel();
	}

	private cancel(): void {
		this.generation += 1;
		if (this.timer !== null) {
			this.timers.clearTimeout(this.timer);
			this.timer = null;
		}
		this.controller?.abort();
		this.controller = null;
		this.inFlight = false;
	}

	private schedule(ms: number): void {
		if (this.disposed || !this.active || this.inFlight) return;
		if (this.timer !== null) this.timers.clearTimeout(this.timer);
		const generation = this.generation;
		this.timer = this.timers.setTimeout(() => {
			this.timer = null;
			if (generation !== this.generation) return;
			void this.tick();
		}, ms);
	}

	private async tick(): Promise<void> {
		if (this.disposed || !this.active || this.inFlight) return;
		const generation = this.generation;
		const controller = new AbortController();
		this.controller = controller;
		this.inFlight = true;
		const deadline = this.timers.setTimeout(() => controller.abort(), this.timeoutMs);
		let outcome: LogFeedFetchOutcome;
		try {
			outcome = await this.options.fetch(this.cursor, controller.signal);
		} catch (error) {
			outcome = {
				kind: 'failed',
				message: controller.signal.aborted ? '時間切れ' : errorMessage(error)
			};
		} finally {
			this.timers.clearTimeout(deadline);
		}
		if (generation !== this.generation || this.disposed) return;
		this.inFlight = false;
		this.controller = null;
		this.apply(outcome);
	}

	private apply(outcome: LogFeedFetchOutcome): void {
		switch (outcome.kind) {
			case 'ok': {
				const { entries, cursor, more } = outcome.response;
				this.cursor = cursor;
				this.rows = mergeLogEntries(this.rows, entries, this.maxRows);
				for (const entry of entries) this.sources.add(entry.source);
				this.status = 'ok';
				this.error = null;
				this.failures = 0;
				this.resets = 0;
				if (more && this.burst < this.maxBurst) {
					this.burst += 1;
					this.schedule(0);
				} else {
					this.burst = 0;
					this.schedule(this.intervalMs);
				}
				break;
			}
			case 'reset': {
				this.cursor = null;
				this.rows = [];
				this.sources = new Set();
				this.burst = 0;
				this.resets += 1;
				this.epoch += 1;
				this.schedule(this.resets > 1 ? this.intervalMs : 0);
				break;
			}
			case 'unauthorized': {
				this.status = 'unauthorized';
				this.error = null;
				break;
			}
			case 'failed': {
				this.failures += 1;
				this.burst = 0;
				this.status = 'retrying';
				this.error = outcome.message;
				this.schedule(this.backoffMs());
				break;
			}
		}
		this.emit();
	}

	/** 失敗が続いたときの間隔（倍々、上限あり）。 */
	backoffMs(): number {
		const factor = 2 ** Math.min(this.failures, 10);
		return Math.min(this.intervalMs * factor, this.maxBackoffMs);
	}

	private emit(): void {
		if (this.disposed) return;
		this.options.onChange(this.snapshot());
	}
}

function errorMessage(error: unknown): string {
	if (error instanceof Error && error.message) return error.message;
	return 'サーバーに接続できません';
}

// --- 表示 --------------------------------------------------------------------

/** 行の目立たせ方（`alert` = 失敗・不確定・拒否・切断）。 */
export type LogRowTone = 'normal' | 'alert';

export interface LogRowView {
	key: string;
	source: LogSource;
	time: string;
	sourceLabel: string;
	target: string;
	result: string;
	tone: LogRowTone;
	who: string;
	description: string;
}

export const LOG_SOURCE_LABELS: Record<LogSource, string> = {
	connection: '接続',
	write: '書き込み',
	operation: '操作'
};

const CONNECTION_KIND_LABELS: Record<string, string> = {
	plc_connected: '接続',
	plc_disconnected: '切断',
	plc_reconnected: '復帰',
	ingest_disconnected: '受け入れの切断'
};

const CONNECTION_ALERT_KINDS = new Set(['plc_disconnected', 'ingest_disconnected']);

const WRITE_RESULT_LABELS: Record<string, string> = {
	ok: '成功',
	failed: '失敗',
	uncertain: '不確定',
	rejected: '拒否',
	suppressed_disabled: '抑止（書き込み無効）',
	suppressed_rate_limited: '抑止（頻度の上限）'
};

const WRITE_ALERT_RESULTS = new Set(['failed', 'uncertain', 'rejected']);

const OPERATION_RESULT_LABELS: Record<string, string> = {
	ok: '成功',
	denied: '拒否',
	failed: '失敗'
};

const OPERATION_ALERT_RESULTS = new Set(['denied', 'failed']);

const DESCRIPTION_MAX = 200;

function pad(n: number, width = 2): string {
	return String(n).padStart(width, '0');
}

/** 時刻の表示（ローカルの `YYYY-MM-DD HH:MM:SS`）。 */
export function formatLogTime(tsMs: number): string {
	const d = new Date(tsMs);
	if (Number.isNaN(d.getTime())) return '--';
	return (
		`${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
		`${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
	);
}

function truncate(text: string): string {
	return text.length > DESCRIPTION_MAX ? `${text.slice(0, DESCRIPTION_MAX)}…` : text;
}

function detailText(detail: unknown): string {
	if (detail === null || detail === undefined) return '';
	if (typeof detail === 'string') return detail;
	try {
		return JSON.stringify(detail);
	} catch {
		return '';
	}
}

/** 1 行の表示（表の列）。 */
export function logRowView(entry: LogFeedEntry): LogRowView {
	const base = {
		key: entry.key,
		source: entry.source,
		time: formatLogTime(entry.ts_ms),
		sourceLabel: LOG_SOURCE_LABELS[entry.source]
	};
	switch (entry.source) {
		case 'connection': {
			const target =
				entry.connection_name ?? (entry.connection_id !== null ? `#${entry.connection_id}` : '—');
			const parts =
				entry.kind === 'ingest_disconnected'
					? [entry.transport, entry.reason].filter((p): p is string => !!p)
					: [entry.detail].filter((p): p is string => !!p);
			return {
				...base,
				target,
				result: CONNECTION_KIND_LABELS[entry.kind] ?? entry.kind,
				tone: CONNECTION_ALERT_KINDS.has(entry.kind) ? 'alert' : 'normal',
				who: '',
				description: truncate(parts.join(' / '))
			};
		}
		case 'write': {
			const value = entry.value !== null ? String(entry.value) : (entry.value_text ?? '');
			const parts = [
				entry.action === 'rate_limit_tripped' ? '頻度の上限に到達' : '',
				value !== '' ? `値 ${value}` : '',
				entry.detail ?? '',
				entry.operator_claim ? `操作者（申告） ${entry.operator_claim}` : ''
			].filter((p) => p !== '');
			return {
				...base,
				target: entry.tag_name,
				result: WRITE_RESULT_LABELS[entry.result] ?? entry.result,
				tone: WRITE_ALERT_RESULTS.has(entry.result) ? 'alert' : 'normal',
				who: entry.api_key_name,
				description: truncate(parts.join(' / '))
			};
		}
		case 'operation': {
			const target = entry.entity_id ? `${entry.resource} ${entry.entity_id}` : entry.resource;
			const detail = detailText(entry.detail);
			const parts = [`${entry.origin} ${entry.action}`, detail].filter((p) => p !== '');
			return {
				...base,
				target,
				result: OPERATION_RESULT_LABELS[entry.result] ?? entry.result,
				tone: OPERATION_ALERT_RESULTS.has(entry.result) ? 'alert' : 'normal',
				who: entry.actor_username ?? '—',
				description: truncate(parts.join(' / '))
			};
		}
	}
}
