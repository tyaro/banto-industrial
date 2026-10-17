/**
 * OPC UA の listener の状態を読み直す係（#634、PR #650 の Codex レビュー P2）。
 *
 * - 保存（PUT）は起動の完了を待たずに返るので、状態が `starting` のあいだだけ
 *   `intervalMs` ごとに読み直す。
 * - 1 回の取得には期限（`timeoutMs`）を付け、`AbortSignal` で打ち切る。取得が
 *   終わらないまま次の予約がされず「起動中…」で止まる、を防ぐ。期限で打ち
 *   切ったときもエラーを知らせ、`starting` なら読み直しを続ける。
 * - 要求に番号を振り、後から出した要求より古い応答は捨てる（保存直後の
 *   読み直しとポーリングが前後しても、古い「起動中」で新しい結果を巻き戻さない）。
 */
import type { OpcUaStatus } from './opcuaSettingsAdmin';

export interface OpcUaStatusPollerOptions {
	/** 状態を 1 回取得する。`signal` が打ち切られたら reject すること。 */
	load: (signal: AbortSignal) => Promise<OpcUaStatus>;
	onStatus: (status: OpcUaStatus) => void;
	onError: (error: unknown) => void;
	/** 1 回の取得の期限（既定 5 秒）。 */
	timeoutMs?: number;
	/** `starting` のあいだの読み直しの間隔（既定 1 秒）。 */
	intervalMs?: number;
}

export class OpcUaStatusPoller {
	private seq = 0;
	private last: OpcUaStatus | null = null;
	private timer: ReturnType<typeof setTimeout> | null = null;
	private stopped = false;
	private readonly timeoutMs: number;
	private readonly intervalMs: number;

	constructor(private readonly options: OpcUaStatusPollerOptions) {
		this.timeoutMs = options.timeoutMs ?? 5000;
		this.intervalMs = options.intervalMs ?? 1000;
	}

	/** いますぐ読み直す（予約済みの読み直しは取り消す）。 */
	async refresh(): Promise<void> {
		this.stopped = false;
		this.clearTimer();
		const seq = ++this.seq;
		const controller = new AbortController();
		const deadline = setTimeout(() => controller.abort(), this.timeoutMs);
		try {
			const status = await this.options.load(controller.signal);
			if (seq !== this.seq || this.stopped) return;
			this.last = status;
			this.options.onStatus(status);
		} catch (error) {
			if (seq !== this.seq || this.stopped) return;
			this.options.onError(error);
		} finally {
			clearTimeout(deadline);
			if (seq === this.seq && !this.stopped && this.last?.state === 'starting') {
				this.timer = setTimeout(() => void this.refresh(), this.intervalMs);
			}
		}
	}

	/** 読み直しをやめる（画面を離れたとき）。飛行中の応答も捨てる。 */
	stop(): void {
		this.stopped = true;
		this.clearTimer();
	}

	private clearTimer(): void {
		if (this.timer !== null) {
			clearTimeout(this.timer);
			this.timer = null;
		}
	}
}
