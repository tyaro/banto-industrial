<script lang="ts">
	/**
	 * 日報画面（#1207 の R3 日報 PR2。recorder-requirements.md §3.5）。表示グループと日付を
	 * 選び、その日（端末のローカル 0:00〜翌 0:00）の 1 時間ごとの平均と 1 日の集計（平均・最大・
	 * 最小）を、組み込みの固定帳票で表示・印刷する（`@banto/report` の `ReportView`）。
	 *
	 * - **帳票は固定**（DB に持たない・編集できない）。テンプレートはタグの本数に合わせて
	 *   `dailyReportLogic.ts` が組み立てる。タグ名などはデータ側に置くので、記号で表が壊れない。
	 * - **日付は今日が既定**（記録は当日の分まで。前の日・次の日・日付欄で動かす）。
	 * - **URL が状態の持ち主**（`?group=<id>&date=YYYY-MM-DD`）。グループ・日付の変更は履歴に積む。
	 * - 読み出しは条件が変わった・画面を離れたときに打ち切り、古い応答は世代で捨てる
	 *   （ヒストリカル画面と同じ作法）。
	 * - 印刷は ReportView の「印刷」ボタン（`window.print()`）。シェルは `app.css` の
	 *   `@media print` が隠し、この画面の操作部は下のスタイルで隠す。
	 */
	import { onMount, untrack } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { ReportView } from '@banto/report';
	import { sessionStore } from '#lib/session.svelte.js';
	import { canWriteResources } from '#lib/permissions.js';
	import { resolveAppPath } from '#lib/navigation.js';
	import {
		getCollectDailyReport,
		isCollectAvailable,
		type CollectDailyReport
	} from '#lib/banto/collectAdmin.js';
	import {
		DEMO_MODE_MESSAGE,
		isTagRegistryAvailable,
		listTags
	} from '#lib/banto/tagRegistryAdmin.js';
	import { displayGroupCatalog } from '#lib/monitor/displayGroupCatalog.svelte.js';
	import {
		INITIAL_TAG_META,
		applyTagMetaLoad,
		parseGroupParam,
		selectGroup,
		tagMetaNotices
	} from '#lib/monitor/monitorLogic.js';
	import PenSelectList from '#lib/penSelection/PenSelectList.svelte';
	import {
		effectivePenSelection,
		needsPenSelection,
		shouldDiscardPenSelection,
		togglePenSelection
	} from '#lib/penSelection/penSelectionLogic.js';
	import {
		DAILY_REPORT_CLIENT_TIMEOUT_MS,
		DAILY_REPORT_EMPTY_MESSAGE,
		DAILY_REPORT_MAX_TAGS,
		dailyReportTimeoutMessage,
		DAILY_REPORT_UNAVAILABLE_MESSAGE,
		buildReportData,
		canShiftNextDay,
		dailyReportErrorMessage,
		dailyReportQuery,
		dailyReportRequestFor,
		isEmptyReport,
		resolveDate,
		shiftDate,
		simulationNote,
		toDateValue,
		unknownTagsNote,
		type ReportTagInfo
	} from '#lib/dailyReport/dailyReportLogic.js';

	const available = isTagRegistryAvailable() && isCollectAvailable();
	const canEdit = $derived(canWriteResources(sessionStore.role) && !sessionStore.publicViewer);

	// --- URL（グループ・日付） ---------------------------------------------------

	/** URL に日付が無いときの「今日」の基準（画面を開いた時刻）。 */
	const mountedAt = Date.now();
	let now = $state(mountedAt);

	const groups = $derived(displayGroupCatalog.groups);
	const requested = $derived(parseGroupParam(page.url.searchParams.get('group')));
	const selection = $derived(selectGroup(groups ?? [], requested, null));
	const selectedGroup = $derived(groups?.find((group) => group.id === selection.id) ?? null);
	const resolved = $derived(resolveDate(page.url.searchParams.get('date'), mountedAt));
	const date = $derived(resolved.date);

	function go(groupId: number, nextDate: string): void {
		now = Date.now();
		void goto(`${resolveAppPath('/daily-report')}${dailyReportQuery(groupId, nextDate)}`, {
			reset: false
		});
	}

	function showGroup(id: number): void {
		go(id, date);
	}

	function onTabKeydown(event: KeyboardEvent, index: number): void {
		if (!groups || groups.length === 0) return;
		let next: number | null = null;
		if (event.key === 'ArrowRight') next = (index + 1) % groups.length;
		else if (event.key === 'ArrowLeft') next = (index - 1 + groups.length) % groups.length;
		else if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = groups.length - 1;
		if (next === null) return;
		event.preventDefault();
		showGroup(groups[next].id);
		document.getElementById(`daily-report-tab-${groups[next].id}`)?.focus();
	}

	// --- 日付の指定 -------------------------------------------------------------

	// URL の日付が変わったら、入力欄を合わせる（戻る・進む・前の日・次の日）。
	let dateDraft = $derived(date);

	function applyDate(): void {
		if (!selectedGroup) return;
		// 空・不正な入力は無視して今の日付に戻す（`resolveDate` が読めないものは今日に倒すため、
		// ここで確かめて URL に載せない）。
		if (shiftDate(dateDraft, 0) === null) {
			dateDraft = date;
			return;
		}
		go(selectedGroup.id, dateDraft);
	}

	function shift(days: number): void {
		const next = shiftDate(date, days);
		if (selectedGroup && next !== null) go(selectedGroup.id, next);
	}

	function today(): void {
		if (selectedGroup) go(selectedGroup.id, toDateValue(new Date()));
	}

	const nextEnabled = $derived(canShiftNextDay(date, now));
	const todayDisabled = $derived(date === toDateValue(new Date(now)));

	// --- タグ情報（名前・単位・小数桁） -------------------------------------------

	let tagMeta = $state(INITIAL_TAG_META);
	const tagMetaNoticeLines = $derived(tagMetaNotices(tagMeta));

	function errorText(err: unknown): string {
		return err instanceof Error ? err.message : String(err);
	}

	async function loadTagMeta(): Promise<void> {
		// 日報は名前・単位・小数桁だけ使う（収集グループ・しきい値は読まない）。
		const meta = await listTags().then(
			(rows) => ({
				ok: true as const,
				value: { tags: rows, collectionGroups: [] }
			}),
			(err: unknown) => ({ ok: false as const, error: errorText(err) })
		);
		tagMeta = applyTagMetaLoad(tagMeta, meta, { ok: true, value: [] });
	}

	async function reload(): Promise<void> {
		await Promise.all([displayGroupCatalog.refresh(), loadTagMeta()]);
	}

	function tagInfoOf(tagId: number): ReportTagInfo {
		const tag = tagMeta.tags.find((t) => t.id === tagId);
		return tag
			? { name: tag.name, unit: tag.unit ? tag.unit : null, decimals: tag.decimals }
			: { name: `タグ ID ${tagId}`, unit: null, decimals: null };
	}

	// --- 日報の読み出し -----------------------------------------------------------

	/**
	 * 日報に載せるペンは 8 本まで（読み出しの口のタグの上限）。グループが 9 本以上のペンを持つ
	 * ときは、ペンのチェックの一覧で選ばせる（既定は先頭から 8 本。選んだ状態はこの画面の中だけで、
	 * グループを替えると既定に戻る。trend-design.md §6）。
	 */
	let chosen = $state<{ groupId: number; tagIds: number[] } | null>(null);
	// 有効なグループが替わったら選択を捨てる（A で選び → B → A に戻っても既定から。URL の戻る・進むも同じ）。
	// 捨てるのは chosen が別のグループのものだったときだけなので、書いたあとは条件が偽になり繰り返さない。
	$effect(() => {
		if (shouldDiscardPenSelection(chosen?.groupId ?? null, selectedGroup?.id ?? null))
			chosen = null;
	});
	const groupTagIds = $derived(selectedGroup ? selectedGroup.pens.map((pen) => pen.tagId) : []);
	const needsSelection = $derived(needsPenSelection(groupTagIds, DAILY_REPORT_MAX_TAGS));
	const penTagIds = $derived(
		effectivePenSelection(
			groupTagIds,
			chosen !== null && selectedGroup !== null && chosen.groupId === selectedGroup.id
				? chosen.tagIds
				: null,
			DAILY_REPORT_MAX_TAGS
		)
	);

	type Outcome =
		| { state: 'ready'; data: CollectDailyReport }
		| { state: 'unavailable' }
		| { state: 'error'; message: string };
	interface Loaded {
		dataKey: string;
		outcome: Outcome;
	}

	let loaded = $state.raw<Loaded | null>(null);
	let retryToken = $state(0);

	const request = $derived(dailyReportRequestFor(date, penTagIds));
	const dataKey = $derived(
		selectedGroup && request
			? `${selectedGroup.id}:${request.fromMs}:${request.toMs}:${request.tagIds.join(',')}`
			: null
	);
	/** 今の要求に対する結果（別の表示のものは使わない）。 */
	const current = $derived(loaded !== null && loaded.dataKey === dataKey ? loaded : null);
	const loading = $derived(request !== null && current === null);

	let generation = 0;

	$effect(() => {
		const key = dataKey;
		void retryToken;
		if (key === null || !available) return;
		const req = untrack(() => request);
		if (req === null) return;
		const mine = ++generation;
		const controller = new AbortController();
		// 通信が止まっても「読み込み中」のまま固まらないよう、期限で打ち切って再試行を出す。
		// 期限が来たら世代を進めて以降の応答（abort の例外を含む）を捨てる。
		const timer = setTimeout(() => {
			if (mine !== generation) return;
			generation += 1;
			controller.abort();
			loaded = { dataKey: key, outcome: { state: 'error', message: dailyReportTimeoutMessage() } };
		}, DAILY_REPORT_CLIENT_TIMEOUT_MS);
		void (async () => {
			let outcome: Outcome;
			try {
				const readout = await getCollectDailyReport(req, controller.signal);
				clearTimeout(timer);
				if (mine !== generation) return;
				outcome =
					readout.state === 'ready'
						? { state: 'ready', data: readout.data }
						: { state: 'unavailable' };
			} catch (err) {
				clearTimeout(timer);
				if (mine !== generation || controller.signal.aborted) return;
				outcome = { state: 'error', message: dailyReportErrorMessage(err) };
			}
			if (mine !== generation) return;
			loaded = { dataKey: key, outcome };
		})();
		return () => {
			// 条件が変わった・画面を離れた: 古い応答は世代で捨て、通信も止める。
			clearTimeout(timer);
			generation += 1;
			controller.abort();
		};
	});

	const outcome = $derived(current?.outcome ?? null);
	// タグ情報が日報の応答より後に届いても、名前・桁を引き直して帳票を作り直す。
	const built = $derived(
		outcome?.state === 'ready' && selectedGroup
			? buildReportData(outcome.data, selectedGroup.name, date, tagInfoOf)
			: null
	);
	const nameOf = (tagId: number): string => tagInfoOf(tagId).name;
	const unknownNote = $derived(
		outcome?.state === 'ready' ? unknownTagsNote(outcome.data.unknownTagIds, nameOf) : null
	);
	const simNote = $derived(
		outcome?.state === 'ready' ? simulationNote(outcome.data, nameOf) : null
	);
	const reportEmpty = $derived(outcome?.state === 'ready' && isEmptyReport(outcome.data));

	onMount(() => {
		if (!available) return;
		void reload();
		const timer = setInterval(() => (now = Date.now()), 30_000);
		return () => clearInterval(timer);
	});
</script>

<div class="page">
	<div class="page-header no-print">
		<h2>日報</h2>
	</div>

	{#if !available}
		<div class="empty-state">
			<p class="empty-message">{DEMO_MODE_MESSAGE}</p>
		</div>
	{:else if groups === null && displayGroupCatalog.error !== null}
		<div class="empty-state" role="alert">
			<p class="empty-message">表示グループを読み込めませんでした</p>
			<p class="empty-hint">{displayGroupCatalog.error}</p>
			<button
				type="button"
				class="secondary"
				onclick={() => void reload()}
				disabled={displayGroupCatalog.loading}
			>
				再読み込み
			</button>
		</div>
	{:else if groups === null}
		<div class="empty-state">
			<p class="empty-hint">表示グループを読み込んでいます。</p>
		</div>
	{:else if groups.length === 0}
		<div class="empty-state">
			<p class="empty-message">表示グループが未設定です</p>
			{#if canEdit}
				<p class="empty-hint">
					<a href={resolveAppPath('/groups')}>表示グループ</a>
					でグループを作成すると、ここで日報を表示・印刷できます。
				</p>
			{:else}
				<p class="empty-hint">
					表示グループは編集者以上のアカウントが「表示グループ」ページで作成します。作成されると、ここで日報を表示・印刷できます。
				</p>
			{/if}
		</div>
	{:else}
		{#if displayGroupCatalog.error !== null}
			<p class="note warn no-print" role="status">
				表示グループの一覧を読み直せませんでした（{displayGroupCatalog.error}）。前に読めた一覧を表示しています。
			</p>
		{/if}
		<div class="tabs no-print" role="tablist" aria-label="表示グループ">
			{#each groups as group, index (group.id)}
				<button
					type="button"
					role="tab"
					id={`daily-report-tab-${group.id}`}
					aria-selected={group.id === selection.id}
					aria-controls="daily-report-panel"
					tabindex={group.id === selection.id ? 0 : -1}
					class:active={group.id === selection.id}
					onclick={() => showGroup(group.id)}
					onkeydown={(event) => onTabKeydown(event, index)}
				>
					{group.name}
				</button>
			{/each}
		</div>

		{#if selection.missingRequested !== null}
			<p class="note warn no-print" role="status">
				指定された表示グループ（ID {selection.missingRequested}）は見つかりませんでした。削除された可能性があります。
			</p>
		{/if}
		{#if resolved.fellBack}
			<p class="note warn no-print" role="status">
				URL の日付を読み取れなかったため、今日を表示しています。
			</p>
		{/if}

		{#if selectedGroup}
			<div
				class="panel"
				id="daily-report-panel"
				role="tabpanel"
				aria-labelledby={`daily-report-tab-${selectedGroup.id}`}
			>
				<div class="date-bar no-print">
					<div class="shift" role="group" aria-label="日付をずらす">
						<button type="button" onclick={() => shift(-1)}>前の日</button>
						<button type="button" onclick={() => shift(1)} disabled={!nextEnabled}>次の日</button>
						<button type="button" onclick={today} disabled={todayDisabled}>今日</button>
					</div>
					<form
						class="custom"
						onsubmit={(event) => {
							event.preventDefault();
							applyDate();
						}}
					>
						<label>
							日付
							<input type="date" bind:value={dateDraft} required />
						</label>
						<button type="submit" class="primary">表示</button>
					</form>
				</div>

				{#if selectedGroup.pens.length === 0}
					<div class="empty-state no-print">
						<p class="empty-message">このグループにはペン（タグ）が割り当てられていません</p>
						{#if canEdit}
							<p class="empty-hint">
								<a href={resolveAppPath('/groups')}>表示グループ</a>でタグを割り当ててください。
							</p>
						{/if}
					</div>
				{:else}
					{#each tagMetaNoticeLines as notice (notice)}
						<p class="note warn no-print" role="status">{notice}</p>
					{/each}
					{#if needsSelection}
						<div class="no-print">
							<PenSelectList
								tagIds={groupTagIds}
								selected={penTagIds}
								limit={DAILY_REPORT_MAX_TAGS}
								label={(id) => tagInfoOf(id).name}
								ontoggle={(id) =>
									(chosen = {
										groupId: selectedGroup.id,
										tagIds: togglePenSelection(groupTagIds, penTagIds, id, DAILY_REPORT_MAX_TAGS)
									})}
							/>
						</div>
						{#if penTagIds.length === 0}
							<p class="note warn no-print" role="status">
								日報に載せるペンを 1 本以上選んでください。
							</p>
						{/if}
					{/if}
					{#if unknownNote !== null}
						<p class="note warn no-print" role="status">{unknownNote}</p>
					{/if}
					{#if simNote !== null}
						<p class="note warn no-print" role="status">{simNote}</p>
					{/if}
					{#if outcome?.state === 'unavailable'}
						<p class="note warn no-print" role="alert">
							{DAILY_REPORT_UNAVAILABLE_MESSAGE}
							<button type="button" class="secondary" onclick={() => (retryToken += 1)}>
								再試行
							</button>
						</p>
					{:else if outcome?.state === 'error'}
						<p class="note warn no-print" role="alert">
							{outcome.message}
							<button type="button" class="secondary" onclick={() => (retryToken += 1)}>
								再試行
							</button>
						</p>
					{/if}
					{#if loading}
						<p class="note no-print" role="status">日報を読み込んでいます。</p>
					{/if}
					{#if reportEmpty}
						<p class="note no-print" role="status">{DAILY_REPORT_EMPTY_MESSAGE}</p>
					{/if}
					{#if built !== null && built.tagCount > 0}
						<ReportView
							template={built.template}
							data={built.data}
							title={`日報_${date}`}
							orientation={built.tagCount > 4 ? 'landscape' : 'portrait'}
						/>
					{/if}
				{/if}
			</div>
		{/if}
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.page-header h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.tabs {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.tabs button {
		padding: 0.35rem 0.9rem;
		border: 1px solid transparent;
		border-bottom: none;
		border-radius: var(--banto-radius) var(--banto-radius) 0 0;
		background: transparent;
		color: var(--banto-text-muted);
		cursor: pointer;
		font: inherit;
	}

	.tabs button.active {
		border-color: var(--banto-border);
		background: var(--banto-surface);
		color: var(--banto-text);
		font-weight: 600;
	}

	.panel {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.date-bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 1rem;
		font-size: 0.85rem;
	}

	.shift,
	.custom {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.35rem;
	}

	.custom label {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		color: var(--banto-text-muted);
	}

	.custom input {
		font: inherit;
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.5rem;
		background: var(--banto-surface);
		color: var(--banto-text);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
	}

	.date-bar button,
	.note button,
	.empty-state button {
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

	.date-bar button:disabled,
	.note button:disabled,
	.empty-state button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.date-bar button:hover:not(:disabled),
	.note button:hover:not(:disabled),
	.empty-state button:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.date-bar button.primary {
		background: var(--banto-primary-solid);
		border-color: var(--banto-primary-solid);
		color: var(--banto-on-solid);
		font-weight: 600;
	}

	.date-bar button.primary:hover:not(:disabled) {
		background: var(--banto-primary-solid);
	}

	.note button {
		margin-left: 0.5rem;
	}

	.note {
		margin: 0;
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	.note.warn {
		color: var(--banto-warning-tint-text);
		background: var(--banto-warning-tint);
		padding: 0.35rem 0.6rem;
		border-radius: var(--banto-radius);
	}

	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		padding: 2rem 0;
		text-align: center;
		color: var(--banto-text-muted);
	}

	.empty-message {
		margin: 0;
		font-size: 1rem;
		font-weight: 600;
		color: var(--banto-text);
	}

	.empty-hint {
		margin: 0;
		font-size: 0.85rem;
		max-width: 32rem;
	}

	.empty-hint a {
		color: var(--banto-primary);
	}

	/* 印刷: この画面の操作部・案内は出さず、帳票（ReportView の紙面）だけにする。 */
	@media print {
		.no-print {
			display: none;
		}
	}
</style>
