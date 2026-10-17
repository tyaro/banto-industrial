import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { OpcUaStatus } from './opcuaSettingsAdmin';
import { OpcUaStatusPoller } from './opcuaStatusPoller';

const status = (state: OpcUaStatus['state']): OpcUaStatus => ({
	state,
	endpoint: state === 'running' ? 'opc.tcp://127.0.0.1:4840/' : null,
	error: null,
	generation: 1
});

/** `signal` が打ち切られるまで終わらない取得（応答しないサーバー）。 */
function hanging(signal: AbortSignal): Promise<OpcUaStatus> {
	return new Promise((_, reject) => {
		signal.addEventListener('abort', () => reject(new Error('aborted')));
	});
}

describe('OPC UA の状態の読み直し（#634、PR #650 の Codex P2）', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('取得が終わらなくても期限で打ち切り、起動中なら読み直しを続ける', async () => {
		const responses: Array<(signal: AbortSignal) => Promise<OpcUaStatus>> = [
			async () => status('starting'),
			hanging,
			async () => status('running')
		];
		const load = vi.fn((signal: AbortSignal) => responses.shift()!(signal));
		const onStatus = vi.fn();
		const onError = vi.fn();
		const poller = new OpcUaStatusPoller({ load, onStatus, onError });

		await poller.refresh();
		expect(onStatus).toHaveBeenLastCalledWith(status('starting'));

		// 1 秒後の読み直しは応答しない。5 秒の期限で打ち切られ、エラーを知らせる。
		await vi.advanceTimersByTimeAsync(1000);
		expect(load).toHaveBeenCalledTimes(2);
		const aborted = load.mock.calls[1][0];
		expect(aborted.aborted).toBe(false);
		await vi.advanceTimersByTimeAsync(5000);
		expect(aborted.aborted).toBe(true);
		expect(onError).toHaveBeenCalledTimes(1);

		// 打ち切った後も、起動中のままなので次を予約している。
		await vi.advanceTimersByTimeAsync(1000);
		expect(load).toHaveBeenCalledTimes(3);
		expect(onStatus).toHaveBeenLastCalledWith(status('running'));

		// 起動済みになったら読み直さない。
		await vi.advanceTimersByTimeAsync(5000);
		expect(load).toHaveBeenCalledTimes(3);
	});

	it('後から出した要求より古い応答は捨てる', async () => {
		let resolveOld: (value: OpcUaStatus) => void = () => {};
		const load = vi
			.fn<(signal: AbortSignal) => Promise<OpcUaStatus>>()
			.mockImplementationOnce(() => new Promise((resolve) => (resolveOld = resolve)))
			.mockImplementationOnce(async () => status('running'));
		const onStatus = vi.fn();
		const poller = new OpcUaStatusPoller({ load, onStatus, onError: vi.fn() });

		const old = poller.refresh();
		await poller.refresh();
		resolveOld(status('starting'));
		await old;
		expect(onStatus).toHaveBeenCalledTimes(1);
		expect(onStatus).toHaveBeenLastCalledWith(status('running'));
	});

	it('stop の後は予約も飛行中の応答も捨てる', async () => {
		const load = vi.fn(async () => status('starting'));
		const onStatus = vi.fn();
		const poller = new OpcUaStatusPoller({ load, onStatus, onError: vi.fn() });
		await poller.refresh();
		poller.stop();
		await vi.advanceTimersByTimeAsync(3000);
		expect(load).toHaveBeenCalledTimes(1);
	});
});
