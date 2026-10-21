/**
 * `ingestStatusView.ts`（R2a-2e、docs/banto-hub-ingest-design.md §9）のユニットテスト。
 */
import { describe, expect, it } from 'vitest';
import {
	SENDER_STATUS_MAX_BYTES,
	describeIngestSender,
	ingestLinkClass,
	ingestLinkLabel,
	pushConnectionStatusLabel,
	truncateSenderStatus
} from './ingestStatusView';

describe('truncateSenderStatus（送り手の status の文を画面でも切り詰める）', () => {
	it('制御文字を落とす', () => {
		expect(truncateSenderStatus('a\nb\tc\u0007d')).toBe('abcd');
	});

	it('1 KiB 以内はそのまま、超えたら文字の境界で切って … を付ける', () => {
		expect(SENDER_STATUS_MAX_BYTES).toBe(1024);
		const exact = 'x'.repeat(1024);
		expect(truncateSenderStatus(exact)).toBe(exact);
		expect(truncateSenderStatus('x'.repeat(2000))).toBe(`${'x'.repeat(1024)}…`);
		// 3 バイトの文字は 341 文字（1,023 バイト）で止まり、1 文字の途中で切らない。
		const cut = truncateSenderStatus('あ'.repeat(400));
		expect(cut).toBe(`${'あ'.repeat(341)}…`);
	});
});

describe('受信の状態の見出し', () => {
	it('live は受信中、down は送り手待ち、無効は無効', () => {
		expect(ingestLinkLabel({ link: 'live', enabled: true })).toBe('受信中');
		expect(ingestLinkLabel({ link: 'down', enabled: true })).toBe('送り手待ち');
		expect(ingestLinkLabel({ link: 'live', enabled: false })).toBe('無効（構成）');
		expect(ingestLinkClass({ link: 'live', enabled: true })).toBe('ok');
		expect(ingestLinkClass({ link: 'down', enabled: true })).toBe('warn');
		expect(ingestLinkClass({ link: 'down', enabled: false })).toBe('muted');
	});

	it('接続の一覧の reconnecting は push では「送り手待ち」', () => {
		expect(pushConnectionStatusLabel('reconnecting')).toBe('送り手待ち');
		expect(pushConnectionStatusLabel('connected')).toBe('受信中');
		expect(pushConnectionStatusLabel('stopped')).toBeNull();
	});
});

describe('describeIngestSender', () => {
	it('WS と REST を書き分け、API キーは名前だけを出す', () => {
		expect(
			describeIngestSender({
				transport: 'ws',
				api_key_name: 'db-bridge',
				peer: '127.0.0.1:53122',
				connected_at: null,
				last_seen_at: null
			})
		).toBe('WebSocket: db-bridge（127.0.0.1:53122） - から接続中');
		expect(
			describeIngestSender({
				transport: 'rest',
				api_key_name: 'script',
				peer: null,
				connected_at: null,
				last_seen_at: null
			})
		).toBe('REST: script 最後の送信 -');
	});
});
