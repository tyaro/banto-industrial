<script lang="ts">
	/**
	 * 「運用モード」カテゴリ（#623 PR-4）。設定モード（この画面を開いている間だけ記録）から
	 * 運用モード（このPCで常時記録）へ切り替える。文言と判断は `modeSwitch.ts`（純関数、
	 * `modeSwitch.test.ts`）で、ここは並べるだけ。
	 *
	 * 受け付けた後はシェルが記録エンジンを止めてプロセスを終え、新しいシェルが切り替えの
	 * 進み具合の窓を出す（`chronogazer_core::mode_switch` の「引き継ぎ」）。この画面はその
	 * 間「切り替えています」を出し、二度と押させない。
	 */
	import {
		AUTO_START_DEFAULT,
		AUTO_START_LABEL,
		CURRENT_MODE_NOTE,
		OPERATION_MODE_NOTE,
		OPERATION_MODE_TITLE,
		PAUSE_NOTE,
		SETTING_MODE_TITLE,
		SWITCHING_MESSAGE,
		autoStartNote,
		canRequest,
		confirmMessage,
		requestSwitchToOperation,
		type SwitchUiState
	} from './modeSwitch';
	import { errorMessage } from './shared';

	let autoStart = $state(AUTO_START_DEFAULT);
	let ui = $state<SwitchUiState>({ kind: 'idle' });

	async function switchToOperation() {
		if (!canRequest(ui)) return;
		if (!window.confirm(confirmMessage(autoStart))) return;
		ui = { kind: 'requesting' };
		try {
			await requestSwitchToOperation(autoStart);
			ui = { kind: 'accepted' };
		} catch (err) {
			ui = { kind: 'failed', message: errorMessage(err) };
		}
	}
</script>

<section>
	<h2>今のモード</h2>
	<p class="current">{SETTING_MODE_TITLE}</p>
	<p class="note">{CURRENT_MODE_NOTE}</p>
</section>

<section>
	<h2>{OPERATION_MODE_TITLE}</h2>
	<p class="note">{OPERATION_MODE_NOTE}</p>
	<label class="toggle">
		<input type="checkbox" bind:checked={autoStart} disabled={!canRequest(ui)} />
		{AUTO_START_LABEL}
	</label>
	<p class="note">{autoStartNote(autoStart)}</p>
	<p class="pause" role="note">{PAUSE_NOTE}</p>
	<div class="save-row">
		<button type="button" onclick={switchToOperation} disabled={!canRequest(ui)}>
			運用モードに切り替える
		</button>
	</div>
	{#if ui.kind === 'requesting'}
		<p class="status" role="status">準備しています…</p>
	{:else if ui.kind === 'accepted'}
		<p class="status" role="status">{SWITCHING_MESSAGE}</p>
	{:else if ui.kind === 'failed'}
		<p class="error" role="alert">{ui.message}</p>
	{/if}
</section>

<style>
	.current {
		font-weight: 600;
		margin: 0 0 0.25rem;
	}
	.pause {
		font-weight: 600;
		margin: 0.5rem 0;
	}
</style>
