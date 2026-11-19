<script lang="ts">
	/**
	 * 「CSV 出力」のダイアログ（#1207 R3 PR3）。表示中の表示グループ・期間を、生の値か
	 * 間隔ごとの統計で書き出す。判断は `exportCsv.ts` の純関数で、ここは出すだけ。
	 *
	 * 中央モーダル（`@banto/ui` の `Modal`）なので Esc・オーバーレイ・× は同じ閉じる処理を
	 * 通る。閉じたら（画面を離れて壊れたときも）結果を無視し、保存には進まない
	 * （Tauri ではサーバー側の読み出しは止まらない）。保存先の選択を取り消したら黙って閉じる。
	 *
	 * CSV は 8 本まで（`EXPORT_MAX_TAGS`）。グループが 9 本以上のペンを持つときは、黙って先頭の
	 * 8 本に絞らず、ペンのチェックの一覧（`PenSelectList`）から選ばせる（trend-design.md §6）。
	 * 選んだ状態はダイアログの中だけで持ち、開き直すと既定（先頭から 8 本）に戻る。
	 */
	import { onDestroy, untrack } from 'svelte';
	import { Modal } from '@banto/ui';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '#lib/toast.svelte.js';
	import { EXPORT_MAX_TAGS, getCollectExport, saveCsvFile } from '#lib/banto/collectAdmin.js';
	import PenSelectList from '#lib/penSelection/PenSelectList.svelte';
	import {
		effectivePenSelection,
		needsPenSelection,
		togglePenSelection
	} from '#lib/penSelection/penSelectionLogic.js';
	import {
		DEFAULT_EXPORT_CHOICE,
		EXPORT_INTERVALS,
		EXPORT_RAW_NOTE,
		EXPORT_STATS,
		EXPORT_UNAVAILABLE_MESSAGE,
		buildExportCsv,
		runExport,
		exportFileName,
		exportKey,
		exportRequestFor,
		snapshotTagInfo,
		exportTimeLabel,
		type ExportChoice,
		type ExportTagInfo
	} from '#lib/historical/exportCsv.js';
	import type { HistoryRange } from '#lib/historical/historyLogic.js';
	import type { ExportStat } from '#lib/banto/collectAdmin.js';

	interface Props {
		open: boolean;
		groupName: string;
		range: HistoryRange;
		tagIds: number[];
		tagInfo: (tagId: number) => ExportTagInfo;
		onclose: () => void;
	}

	let { open, groupName, range, tagIds, tagInfo, onclose }: Props = $props();

	let mode = $state<'raw' | 'interval'>('interval');
	let intervalMs = $state(
		DEFAULT_EXPORT_CHOICE.mode === 'interval' ? DEFAULT_EXPORT_CHOICE.intervalMs : 60_000
	);
	let stat = $state<ExportStat>(
		DEFAULT_EXPORT_CHOICE.mode === 'interval' ? DEFAULT_EXPORT_CHOICE.stat : 'avg'
	);
	// 9 本以上のグループで選んだタグ（`null` = 既定の選択）。
	let chosen = $state<number[] | null>(null);
	const needsSelection = $derived(needsPenSelection(tagIds.length, EXPORT_MAX_TAGS));
	const selectedTagIds = $derived(effectivePenSelection(tagIds, chosen, EXPORT_MAX_TAGS));
	let busy = $state(false);
	let errorText = $state<string | null>(null);
	let controller: AbortController | null = null;

	const choice = $derived<ExportChoice>(
		mode === 'raw' ? { mode: 'raw' } : { mode: 'interval', intervalMs, stat }
	);

	// 開いた時点の対象の鍵。開いている間にグループ・期間・タグが変わったら（ブラウザの
	// 戻る・進む）、表示中の期間と食い違うので読み出しを止めて閉じる。
	let openKey: string | null = null;

	// 開き直したら前回のエラーを消し、閉じたら通信を止める。
	$effect(() => {
		const key = exportKey(groupName, range, tagIds);
		if (!open) {
			controller?.abort();
			controller = null;
			busy = false;
			openKey = null;
		} else if (openKey === null) {
			openKey = key;
			errorText = null;
			chosen = null;
		} else if (openKey !== key) {
			openKey = null;
			controller?.abort();
			controller = null;
			busy = false;
			untrack(() => onclose());
		}
	});

	// 画面を離れて壊れたとき（open が false にならないまま）も読み出しを無効にする。
	onDestroy(() => {
		controller?.abort();
		controller = null;
	});

	function messageOf(err: unknown): string {
		if (isProviderError(err)) {
			if (err.body.kind === 'validation') {
				const lines = err.body.field_errors.map((e) => e.message);
				if (lines.length > 0) return lines.join(' / ');
			}
			return err.message;
		}
		return err instanceof Error ? err.message : String(err);
	}

	async function run(): Promise<void> {
		if (busy || selectedTagIds.length === 0) return;
		busy = true;
		errorText = null;
		const mine = new AbortController();
		controller = mine;
		// 始めた時点の値で組む（途中で画面側が変わっても、応答に合う名前・見出しにする）。
		const snapGroup = groupName;
		const snapRange = range;
		const snapTagIds = [...selectedTagIds];
		const snapChoice = choice;
		const snapInfo = snapshotTagInfo(snapTagIds, tagInfo);
		try {
			const outcome = await runExport({
				signal: mine.signal,
				fetch: () =>
					getCollectExport(exportRequestFor(snapRange, snapTagIds, snapChoice), mine.signal),
				save: async (readout) => {
					if (readout.state !== 'ready') return { kind: 'unavailable' as const };
					const csv = buildExportCsv(readout.data, snapInfo);
					return {
						kind: 'saved' as const,
						saved: await saveCsvFile(exportFileName(snapGroup, snapChoice), csv)
					};
				}
			});
			if (outcome.kind === 'cancelled') return;
			if (outcome.result.kind === 'unavailable') {
				errorText = EXPORT_UNAVAILABLE_MESSAGE;
				return;
			}
			const saved = outcome.result.saved;
			if (!saved.saved) {
				onclose();
				return;
			}
			toastStore.push(
				'success',
				saved.fileName ? `${saved.fileName} に保存しました` : 'CSV を出力しました'
			);
			onclose();
		} catch (err) {
			errorText = messageOf(err);
		} finally {
			if (controller === mine) controller = null;
			if (!mine.signal.aborted) busy = false;
		}
	}
</script>

<Modal {open} title="CSV 出力" width="480px" {onclose}>
	<form
		class="export"
		onsubmit={(event) => {
			event.preventDefault();
			void run();
		}}
	>
		<dl class="summary">
			<dt>表示グループ</dt>
			<dd>{groupName}</dd>
			<dt>期間</dt>
			<dd>{exportTimeLabel(range.fromMs, false)} 〜 {exportTimeLabel(range.toMs, false)}</dd>
		</dl>

		{#if needsSelection}
			<PenSelectList
				{tagIds}
				selected={selectedTagIds}
				limit={EXPORT_MAX_TAGS}
				label={(id) => tagInfo(id).name}
				disabled={busy}
				ontoggle={(id) =>
					(chosen = togglePenSelection(tagIds, selectedTagIds, id, EXPORT_MAX_TAGS))}
			/>
		{/if}

		<fieldset class="choice" disabled={busy}>
			<legend>出力する値</legend>
			<label class="radio">
				<input type="radio" name="export-mode" value="raw" bind:group={mode} />
				生の値
			</label>
			{#if mode === 'raw'}
				<p class="note">{EXPORT_RAW_NOTE}</p>
			{/if}
			<label class="radio">
				<input type="radio" name="export-mode" value="interval" bind:group={mode} />
				間隔ごとの統計
			</label>
			{#if mode === 'interval'}
				<div class="interval">
					<label>
						間隔
						<select bind:value={intervalMs}>
							{#each EXPORT_INTERVALS as option (option.ms)}
								<option value={option.ms}>{option.label}</option>
							{/each}
						</select>
					</label>
					<label>
						統計
						<select bind:value={stat}>
							{#each EXPORT_STATS as option (option.stat)}
								<option value={option.stat}>{option.label}</option>
							{/each}
						</select>
					</label>
				</div>
			{/if}
		</fieldset>

		{#if errorText !== null}
			<p class="error" role="alert">{errorText}</p>
		{/if}

		<div class="actions">
			<button type="button" onclick={onclose}>キャンセル</button>
			<button type="submit" class="primary" disabled={busy || selectedTagIds.length === 0}>
				{busy ? '出力しています…' : '出力'}
			</button>
		</div>
	</form>
</Modal>

<style>
	.export {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.summary {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 0.25rem 0.75rem;
		margin: 0;
		font-size: 0.85rem;
	}

	.summary dt {
		color: var(--banto-text-muted);
	}

	.summary dd {
		margin: 0;
	}

	.choice {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		padding: 0.5rem 0.75rem;
	}

	.radio {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.interval {
		display: flex;
		gap: 1rem;
		flex-wrap: wrap;
		padding-left: 1.4rem;
	}

	.interval label {
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}

	.interval select {
		font: inherit;
		height: var(--banto-control-height);
		box-sizing: border-box;
		background: var(--banto-surface);
		color: var(--banto-text);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
	}

	.note {
		margin: 0;
		padding-left: 1.4rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.error {
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.85rem;
	}

	.actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}

	.actions button {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.75rem;
		background: transparent;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		font: inherit;
		cursor: pointer;
	}

	.actions button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.actions button.primary {
		background: var(--banto-primary-solid);
		border-color: var(--banto-primary-solid);
		color: var(--banto-on-solid);
		font-weight: 600;
	}
</style>
