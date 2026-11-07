<script lang="ts">
	/**
	 * 接続の編集 Drawer の「PLC の時計」（H2 #1065・H3 #1068 の Hub の REST の画面）。
	 *
	 * - 表示: `GET /api/plc-connections/{id}/clock`（PLC の時刻・この PC の時刻・差）。
	 *   **自動のポーリングはしない**。読むたびに PLC と通信するので、開いただけでは読まず、
	 *   ボタンを押したときだけ読む（サーバー側も 5 秒は直近の結果を使い回す）。
	 * - 設定: `PUT .../clock-sync`。接続の「保存」（保留キュー・適用）とは別に**即時に保存**される。
	 *   未保存の変更は `dirty`（`$bindable`）で親に伝え、Drawer が Esc・外側のクリックで
	 *   閉じないようにする。
	 * - 手動: `POST .../clock-sync`。PLC の時計を書き換える。REST は Hub の `enabled`
	 *   （定期の設定）を問わないので、ここでも定期の有効・無効や設定の未保存と無関係に押せる。
	 *   確認ダイアログで危険（SNTP・GOT との上書き、イベント履歴 24000）を示す。
	 * - 直近の結果（`last`）は Hub のメモリだけ（再起動で消える）。
	 * - 古い応答は捨てる: 要求ごとの番号と、`connectionId` が変わるたび（・破棄のとき）に進める
	 *   世代で、応答が来た時点で今の接続・今の要求のものでなければ状態へ書かない
	 *   （`ConnectionDrawer.svelte` の `rangeDiagnosticRequest` と同じ流儀）。
	 */
	import { untrack } from 'svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '#lib/toast.svelte.js';
	import {
		getClockSync,
		putClockSync,
		readPlcClock,
		runClockSync
	} from '#lib/banto/tagRegistryAdmin.js';
	import {
		clockReadResultLabel,
		clockSyncResultLabel,
		deviceCodeSuffix,
		formatOffset,
		isClockSyncFormDirty,
		plcTimeText,
		readingDiffText,
		scheduleSummary,
		settingsToForm,
		validateClockSyncForm,
		type ClockReadView,
		type ClockSyncForm,
		type ClockSyncFormErrors,
		type ClockSyncRecord,
		type ClockSyncSettings
	} from '#lib/banto/plcClockAdmin.js';
	import { formatSqliteUtc } from '#lib/banto/sqliteTime.js';

	let {
		connectionId,
		// 親が読む出力用の bindable（この部品の中では書くだけ）。
		// eslint-disable-next-line no-useless-assignment
		dirty = $bindable()
	}: {
		connectionId: number;
		/** 時計合わせの設定に未保存の変更があるか（読み込んだ設定とフォームが違う）。 */
		dirty?: boolean;
	} = $props();

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const blankForm = (): ClockSyncForm => ({ enabled: false, kind: 'hourly', hour: 0, minute: 5 });

	let baseline: ClockSyncSettings | null = $state(null);
	let form: ClockSyncForm = $state(blankForm());
	let formErrors: ClockSyncFormErrors = $state({});
	let last: ClockSyncRecord | null = $state(null);
	let loading = $state(true);
	let loadError: string | null = $state(null);
	let saving = $state(false);
	let running = $state(false);
	let reading = $state(false);
	let readResult: ClockReadView | null = $state(null);
	/** 設定の欄と保存は、読込み中・保存中・読込み失敗（基準値なし）の間は触れない。 */
	const settingsLocked = $derived(loading || saving || baseline === null);
	const idPrefix = $derived(`plc-clock-${connectionId}`);

	// 世代: 接続が変わる・部品が破棄されるたびに進める（遅れた応答を捨てる）。
	let generation = 0;
	let loadRequest = 0;
	let saveRequest = 0;
	let runRequest = 0;
	let readRequest = 0;

	function reset(): void {
		baseline = null;
		form = blankForm();
		formErrors = {};
		last = null;
		loading = true;
		loadError = null;
		saving = false;
		running = false;
		reading = false;
		readResult = null;
	}

	async function load(): Promise<void> {
		const id = connectionId;
		const gen = generation;
		const request = ++loadRequest;
		const isCurrent = () => gen === generation && id === connectionId && request === loadRequest;
		loading = true;
		loadError = null;
		try {
			const view = await getClockSync(id);
			if (!isCurrent()) return;
			baseline = view.settings;
			form = settingsToForm(view.settings);
			formErrors = {};
			last = view.last;
		} catch (err) {
			if (isCurrent()) loadError = errorMessage(err);
		} finally {
			if (isCurrent()) loading = false;
		}
	}

	$effect(() => {
		void connectionId;
		untrack(() => {
			generation += 1;
			reset();
			void load();
		});
		return () => {
			generation += 1;
		};
	});

	$effect(() => {
		dirty = baseline !== null && isClockSyncFormDirty(baseline, form);
	});
	// 部品が消えた後に未保存の印を親へ残さない。
	$effect(() => () => {
		dirty = false;
	});

	async function readClock(): Promise<void> {
		if (reading) return;
		const id = connectionId;
		const gen = generation;
		const request = ++readRequest;
		const isCurrent = () => gen === generation && id === connectionId && request === readRequest;
		reading = true;
		try {
			const result = await readPlcClock(id);
			if (isCurrent()) readResult = result;
		} catch (err) {
			if (isCurrent()) toastStore.push('error', errorMessage(err));
		} finally {
			if (isCurrent()) reading = false;
		}
	}

	async function saveSettings(): Promise<void> {
		// 読めていない既定値で上書き保存させない。
		if (saving || loading || baseline === null) return;
		const parsed = validateClockSyncForm(form);
		if (!parsed.ok) {
			formErrors = parsed.errors;
			return;
		}
		formErrors = {};
		const id = connectionId;
		const gen = generation;
		const request = ++saveRequest;
		const isCurrent = () => gen === generation && id === connectionId && request === saveRequest;
		saving = true;
		try {
			const view = await putClockSync(id, parsed.settings);
			if (!isCurrent()) return;
			baseline = view.settings;
			form = settingsToForm(view.settings);
			last = view.last;
			toastStore.push('success', '時計合わせの設定を保存しました');
		} catch (err) {
			if (isCurrent()) toastStore.push('error', errorMessage(err));
		} finally {
			if (isCurrent()) saving = false;
		}
	}

	async function runNow(): Promise<void> {
		if (running) return;
		if (
			!window.confirm(
				'PLC の時計をこの PC の現地時刻に書き換えます。PLC 自身の SNTP・PLC のプログラム・GOT の時刻同期などが同じ時計を合わせていると互いに上書きします。実行のたびに PLC のイベント履歴に「時計設定」（イベントコード 24000）が残ります。実行しますか？'
			)
		) {
			return;
		}
		const id = connectionId;
		const gen = generation;
		const request = ++runRequest;
		const isCurrent = () => gen === generation && id === connectionId && request === runRequest;
		running = true;
		try {
			const record = await runClockSync(id);
			if (isCurrent()) last = record;
		} catch (err) {
			if (isCurrent()) toastStore.push('error', errorMessage(err));
		} finally {
			if (isCurrent()) running = false;
		}
	}
</script>

<section class="test-connection clock" aria-label="PLC の時計">
	<h4>PLC の時計</h4>

	<div class="block">
		<button type="button" class="test-btn" onclick={readClock} disabled={reading}>
			{reading ? '読取り中…' : 'PLC の時計を読む'}
		</button>
		{#if readResult}
			<div class="test-result" data-testid="clock-read-result">
				{#if readResult.result === 'ok' && readResult.plcTime !== null}
					<div>PLC: {plcTimeText(readResult.plcTime)}</div>
					<div>この PC: {plcTimeText(readResult.pcTime)}</div>
					{#if readResult.offsetSeconds !== null}
						<div>差: {formatOffset(readResult.offsetSeconds)}</div>
					{/if}
					<div class="hint">差は ±1 秒の誤差があります</div>
				{:else}
					<div class="error">
						{clockReadResultLabel(readResult.result)}{deviceCodeSuffix(
							readResult.result,
							readResult.code
						)}
					</div>
					{#if readResult.message}<div>{readResult.message}</div>{/if}
				{/if}
				{#if readResult.cached}
					<div class="hint">（5 秒以内の前回の結果）</div>
				{/if}
			</div>
		{/if}
	</div>

	<div class="block">
		<h5>時計合わせの設定</h5>
		{#if loadError}
			<div class="test-result error" role="alert">
				<span>{loadError}</span>
				<button type="button" class="secondary" onclick={() => void load()} disabled={loading}>
					再読込み
				</button>
			</div>
		{/if}
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} disabled={settingsLocked} />
			時刻表で定期的に合わせる
		</label>
		<div class="schedule">
			<div class="field">
				<label for="{idPrefix}-kind">時刻表</label>
				<select id="{idPrefix}-kind" bind:value={form.kind} disabled={settingsLocked}>
					<option value="hourly">毎時</option>
					<option value="daily">毎日</option>
				</select>
			</div>
			{#if form.kind === 'daily'}
				<div class="field">
					<label for="{idPrefix}-hour">時</label>
					<input
						id="{idPrefix}-hour"
						type="number"
						min="0"
						max="23"
						step="1"
						bind:value={form.hour}
						disabled={settingsLocked}
					/>
					{#if formErrors.hour}<span class="err">{formErrors.hour}</span>{/if}
				</div>
			{/if}
			<div class="field">
				<label for="{idPrefix}-minute">分</label>
				<input
					id="{idPrefix}-minute"
					type="number"
					min="0"
					max="59"
					step="1"
					bind:value={form.minute}
					disabled={settingsLocked}
				/>
				{#if formErrors.minute}<span class="err">{formErrors.minute}</span>{/if}
			</div>
		</div>
		<button type="button" class="test-btn" onclick={saveSettings} disabled={settingsLocked}>
			{saving ? '保存中…' : '時計合わせの設定を保存'}
		</button>
		<p class="hint">
			この設定は接続の「保存」とは別に、すぐに反映されます（保留中の変更・適用を通りません）。時刻はこの
			PC の現地時刻です。Hub の停止中などで過ぎた回は後から実行しません。
		</p>
		<p class="hint warn">
			PLC 自身の SNTP・PLC のプログラム・GOT
			の時刻同期などが同じ時計を合わせていると互いに上書きします。毎回 PLC
			のイベント履歴に「時計設定」（イベントコード
			24000）が残るので、間隔を短くしすぎないでください。
		</p>
		<p class="hint">
			実際に書くには、ドライバの接続設定で時計合わせ（clock_set）を有効にし、書込みを許可したドライバである必要があります。
		</p>
	</div>

	<div class="block">
		<button type="button" class="test-btn danger" onclick={runNow} disabled={running}>
			{running ? '時計合わせ中…' : '今すぐ PLC の時計を合わせる'}
		</button>
	</div>

	<div class="block" data-testid="clock-last">
		<h5>直近の結果</h5>
		{#if last}
			<div class="test-result">
				<div>
					実行: {formatSqliteUtc(last.at)}（{last.trigger === 'schedule' ? '定期' : '手動'}）
				</div>
				<div>
					結果: {clockSyncResultLabel(last.result)}{deviceCodeSuffix(last.result, last.code)}
				</div>
				{#if last.time}<div>渡した時刻: {plcTimeText(last.time)}</div>{/if}
				{#if last.message}<div>{last.message}</div>{/if}
				{#if last.before}<div>合わせる前の差: {readingDiffText(last.before)}</div>{/if}
				{#if last.after}<div>合わせた後の差: {readingDiffText(last.after)}</div>{/if}
			</div>
		{:else}
			<p class="hint">まだ実行していません（Hub の再起動で消えます）。</p>
		{/if}
		{#if baseline}
			<p class="hint">
				現在の設定: {baseline.enabled ? '有効' : '無効'}・{scheduleSummary(baseline.schedule)}
			</p>
		{/if}
	</div>
</section>

<style>
	.test-connection {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		margin-bottom: 0.75rem;
	}

	h4,
	h5 {
		margin: 0;
	}

	h4 {
		font-size: 0.9rem;
	}

	h5 {
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.block {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.5rem;
	}

	.schedule {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.field.checkbox {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
	}

	.field input,
	.field select {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
	}

	.field.checkbox input {
		width: auto;
	}

	.test-btn {
		display: inline-flex;
		flex-shrink: 0;
		white-space: nowrap;
		align-items: center;
		gap: 0.4rem;
		background: var(--banto-bg);
		color: var(--banto-text);
		border: 1px solid var(--banto-border);
		font-weight: 500;
	}

	.test-btn:hover:not(:disabled) {
		background: var(--banto-surface);
	}

	.test-btn.danger {
		border-color: var(--banto-danger);
		color: var(--banto-danger);
	}

	.test-result {
		font-size: 0.8rem;
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.test-result.error,
	.error {
		color: var(--banto-danger);
		font-weight: 600;
	}

	.hint {
		margin: 0;
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	.hint.warn {
		color: var(--banto-warning);
	}

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
	}

	button {
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	button:hover:not(:disabled) {
		background: var(--banto-primary-hover);
	}

	button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text-muted);
	}
</style>
