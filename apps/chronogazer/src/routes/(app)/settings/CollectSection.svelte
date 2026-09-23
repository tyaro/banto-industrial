<script lang="ts">
	/**
	 * 収集カテゴリ（#383 段階2b / R1-C の C-3b）。**全ロールが開ける**
	 * （`+layout.ts` の `visible.collect` 参照）。
	 *
	 * この section がやることは 3 つだけで、**新しい API も新しい状態語彙も
	 * 足していない**（口は C-1〜C-3a = #406/#407/#408 で入ったものをそのまま
	 * 使う）:
	 *
	 * 1. **5 状態をそのまま見せる**（`stopped`/`starting`/`running`/`noTargets`/
	 *    `startFailed`）。**`startFailed` の理由は状態に載っていない**ので、
	 *    無いものを出そうとしない - 代わりに「操作すると理由が返る」ことを
	 *    説明文に書き、実際に操作したときの `reason` を
	 *    `collectOperationDisplay` が必ず出す（`collectAdmin.ts` の doc）。
	 * 2. **接続ごとの状態の 3 つの結末を別々に見せる**（走っていない / 読めな
	 *    かった / 読めて 0 件）。`unavailable` を「0 件」や「接続なし」に潰さない
	 *    （docs/implementation-checklist.md §5）。
	 * 3. **操作（開始・停止・再起動）は editor 以上にだけ出す**。`disabled` では
	 *    なく**出さない** - `routes/(app)/tags/+page.svelte` が viewer に新規
	 *    作成・削除を出さないのと同じ「見せない」方針に合わせる。
	 *
	 * ポーリングの規律は `HubSection.svelte`（#332/#383 段階1 のレビューで
	 * 作り込んだもの）にそのまま倣う。発明はしていない:
	 *
	 * - **恒久的に失敗したら、そう言う**。連続失敗を数え（成功で 0 に戻す）、
	 *   `COLLECT_POLL_FAILURE_LIMIT` 回で「状態を取得できていません」に切り替える。
	 *   **表は消さない**（消すと「0 件」に潰れて別の嘘になる）ので、最後に取得
	 *   できた時刻を添える。**トーストは出さない**（明示操作のエラー表示を
	 *   上書きしない）。
	 * - **飛行中の応答が新しい状態を巻き戻さない**（`isPollResultFresh` =
	 *   明示操作との競合、`isPollGenerationCurrent` = 停止と再開の競合）。
	 * - **1 回の読み取りに上限**（`COLLECT_READ_TIMEOUT_MS`）。**reject では
	 *   なく無応答**の相手だと、上限が無ければ失敗も数えられずポーリングの
	 *   ループごと止まる。
	 * - **明示操作にも上限**（`COLLECT_UI_TIMEOUT_MS`）。**打ち切りは「失敗」
	 *   ではない**ので、文言を分ける。
	 * - ページを離れたら止める。タブが隠れている間も止める。
	 *
	 * 現在値（`collect_values`）はこの画面では扱わない - 値の表示は R1-D の
	 * 監視画面。イベント一覧は `/events`（同じ PR）。
	 *
	 * #414 段階2: 開始時に不正な設定だけを外して残りを動かすようになったので、
	 * 状態が持つ除外の一覧（`exclusions`）を「除外あり（N 件）」の見出しと
	 * 表（種類・名前・理由）で出し、直す場所（`/tags`）へのリンクを添える。
	 * 判定も文言も Rust が作ったもので、ここでは並べるだけ。
	 *
	 * #413: 接続ごとの状態の行に「シミュレーション中（値は記録されません）」を
	 * 添える。出すのは**走っている収集が**その接続をシミュレータ相手に動かして
	 * いるときだけ（`ConnectionView.simulation`。レジストリの今の値ではない -
	 * 切替を保存しただけでは、再起動までこの表示は変わらない）。
	 */
	import { onDestroy, onMount } from 'svelte';
	import { canWriteResources } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';
	import {
		COLLECT_READ_TIMEOUT_MS,
		COLLECT_UI_TIMEOUT_MS,
		collectActionLabel,
		collectConnectionsNote,
		collectExclusions,
		collectExclusionsHeadline,
		collectExclusionsNote,
		exclusionUnitLabel,
		collectStateDetail,
		collectStateHeadline,
		collectStaleNote,
		connectionSimulationNote,
		connectionStatusLabel,
		getCollectConnections,
		getCollectStatus,
		isCollectAvailable,
		isCollectStale,
		isPollGenerationCurrent,
		isPollResultFresh,
		nextPollFailureCount,
		runCollectAction,
		runWithLimit,
		collectOperationDisplay,
		toCollectStateView,
		type CollectAction,
		type CollectorStateView,
		type ConnectionView,
		type Readout
	} from '$lib/banto/collectAdmin';
	import { errorMessage } from './shared';

	const available = isCollectAvailable();
	/** 操作は editor 以上（`chronogazer_core::collect::COLLECT_OPERATION_ROLE`）。 */
	const canOperate = $derived(canWriteResources(sessionStore.role));

	/** ポーリング間隔（ms）。状態はキューもディスクも触らない読み取り。 */
	const COLLECT_POLL_MS = 2000;

	const ACTIONS: CollectAction[] = ['start', 'stop', 'restart'];

	/**
	 * まだ一度も読めていない間は `null`。**既定値を置かない**のが要点で、
	 * `{ state: 'stopped' }` から始めると「読めていない」が「停止している」に
	 * 化ける（`Readout` で潰さないと決めたことを、画面の初期値で潰してしまう）。
	 */
	let status = $state<CollectorStateView | null>(null);
	let connections = $state<Readout<Record<string, ConnectionView>> | null>(null);

	/** 連続失敗回数（成功で 0 に戻る）と、最後に取得できた時刻。表示ごとに別。 */
	let statusFailures = $state(0);
	let connectionsFailures = $state(0);
	let lastStatusAt = $state<number | null>(null);
	let lastConnectionsAt = $state<number | null>(null);
	const statusStale = $derived(isCollectStale(statusFailures));
	const connectionsStale = $derived(isCollectStale(connectionsFailures));
	/** まだ一度も読めていないことを、状態名のどれかに寄せて言い切らない。 */
	const statusHeadline = $derived(
		status === null ? 'まだ取得できていません' : collectStateHeadline(status, statusStale)
	);

	/** #414 段階2: 開始時に外した設定（状態が持つ一覧。読めていなければ空）。 */
	const exclusions = $derived(status === null ? [] : collectExclusions(status));
	const exclusionsHeadline = $derived(collectExclusionsHeadline(exclusions.length));
	const exclusionsNote = $derived(status === null ? null : collectExclusionsNote(status));

	const connectionEntries = $derived(
		connections?.state === 'ready' ? Object.entries(connections.data) : []
	);

	let busy = $state(false);
	/** 明示操作の結果（受付・完了・打ち切り）。エラーとは別の行に出す。 */
	let opNotice = $state<string | null>(null);
	let opError = $state<string | null>(null);

	/**
	 * 明示操作の結果を何回反映したか。飛行中のポーリングは送信時の番号を覚えて
	 * おき、着いたときに変わっていたら自分の応答を捨てる（`isPollResultFresh`）。
	 */
	let appliedSeq = 0;

	function beginExplicitChange(): void {
		appliedSeq += 1;
	}

	function markStatusFresh(): void {
		statusFailures = nextPollFailureCount(statusFailures, 'ok');
		lastStatusAt = Date.now();
	}

	function markConnectionsFresh(): void {
		connectionsFailures = nextPollFailureCount(connectionsFailures, 'ok');
		lastConnectionsAt = Date.now();
	}

	/**
	 * 明示操作の応答に載っていた状態を反映する（ポーリングを待たない）。
	 *
	 * 押した直後に前の状態が 2 秒残ると「押しても何も起きない」ように見える。
	 * **理由（`reason`）はここで落とす** - 状態の表示は公開用の形だけを扱い、
	 * 理由は操作の結果の文言（`collectOperationDisplay`）だけが出す。
	 */
	function applyOperationStatus(next: CollectorStateView): void {
		beginExplicitChange();
		status = next;
		markStatusFresh();
	}

	async function operate(action: CollectAction): Promise<void> {
		busy = true;
		opNotice = null;
		opError = null;
		// 操作中に飛んでいたポーリングの応答は、もう古い。
		beginExplicitChange();
		try {
			const outcome = await runWithLimit(
				(signal) => runCollectAction(action, signal),
				COLLECT_UI_TIMEOUT_MS
			);
			const display = collectOperationDisplay(
				action,
				outcome,
				outcome.kind === 'failed' ? errorMessage(outcome.error) : null
			);
			opNotice = display.notice;
			opError = display.error;
			// 打ち切り（`timedOut`）のときは状態を書き換えない - 何も返って
			// きていないので、書ける新しい事実が無い。続きはポーリングが拾う。
			if (outcome.kind === 'ok') applyOperationStatus(toCollectStateView(outcome.value.status));
		} finally {
			busy = false;
		}
	}

	// --- ポーリング（`HubSection.svelte` と同じ作法） --------------------------

	let pollTimer: ReturnType<typeof setTimeout> | null = null;
	let polling = false;
	/**
	 * 停止のたびに進む世代。飛行中だった要求は送信時の世代を持ち、**応答の
	 * 適用も次回の予約も**世代が現役のときしかしない（これが無いと、停止した
	 * 瞬間に飛んでいた要求が次のタイマを張り、再開後のループと二重に回る）。
	 */
	let pollGeneration = 0;

	/**
	 * 1 周期ぶんの読み取り。状態 → 接続ごとの状態の順に、**それぞれ上限付きで**
	 * 直列に読む。直列なのは `HubSection.svelte` と同じ理由（並べて投げると、
	 * 遅れた古い応答が新しい応答の後に着いて表示を巻き戻せる）。
	 *
	 * 失敗も打ち切りも**トーストにしない**。どちらも「今の状態を取得できて
	 * いない」で、数える以上のことをしない（明示操作のエラー表示を上書きしない）。
	 */
	async function pollOnce(generation: number): Promise<void> {
		const sentAt = appliedSeq;
		const statusOutcome = await runWithLimit(
			(signal) => getCollectStatus(signal),
			COLLECT_READ_TIMEOUT_MS
		);
		if (!isPollGenerationCurrent(generation, pollGeneration)) return;
		if (statusOutcome.kind === 'ok') {
			// 読めた事実は、その値を採用するかどうかとは別（明示操作の結果を
			// 優先して捨てる場合でも、状態は取得できている）。
			markStatusFresh();
			if (isPollResultFresh(sentAt, appliedSeq)) status = statusOutcome.value;
		} else {
			statusFailures = nextPollFailureCount(statusFailures, 'failed');
		}

		const connectionsSentAt = appliedSeq;
		const connectionsOutcome = await runWithLimit(
			(signal) => getCollectConnections(signal),
			COLLECT_READ_TIMEOUT_MS
		);
		if (!isPollGenerationCurrent(generation, pollGeneration)) return;
		if (connectionsOutcome.kind === 'ok') {
			markConnectionsFresh();
			if (isPollResultFresh(connectionsSentAt, appliedSeq)) connections = connectionsOutcome.value;
		} else {
			connectionsFailures = nextPollFailureCount(connectionsFailures, 'failed');
		}
	}

	/**
	 * 1 周期読んでから次を予約する。予約が読み取りの**完了に依存している**
	 * ので、上限が無いとループごと止まる（上限を入れてあるので必ず戻る）。
	 */
	async function pollThenSchedule(generation: number): Promise<void> {
		await pollOnce(generation);
		if (!isPollGenerationCurrent(generation, pollGeneration)) return;
		pollTimer = setTimeout(() => void pollThenSchedule(generation), COLLECT_POLL_MS);
	}

	function stopPolling(): void {
		pollGeneration += 1;
		polling = false;
		if (pollTimer !== null) {
			clearTimeout(pollTimer);
			pollTimer = null;
		}
	}

	function startPolling(): void {
		if (!available || polling) return;
		polling = true;
		void pollThenSchedule(pollGeneration);
	}

	/** タブが隠れている間は止める（見ていない画面のために回し続けない）。 */
	function onVisibilityChange(): void {
		if (document.visibilityState === 'visible') {
			startPolling();
		} else {
			stopPolling();
		}
	}

	onMount(() => {
		if (!available) return;
		document.addEventListener('visibilitychange', onVisibilityChange);
		if (document.visibilityState === 'visible') startPolling();
	});

	onDestroy(() => {
		stopPolling();
		if (typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', onVisibilityChange);
		}
	});
</script>

<section>
	<h2>収集</h2>

	{#if available}
		<p class="status">
			状態: <strong>{statusHeadline}</strong>
		</p>
		<!--
			恒久的に失敗したら、そう言う。表示は消さず「いつ取得できた表示か」を
			添える（トーストは出さない - 明示操作のエラー表示を上書きしない）。
		-->
		{#if statusStale}
			<p class="note poll-stale" role="status">{collectStaleNote('status', lastStatusAt)}</p>
		{/if}
		{#if status}
			<p class="note">{collectStateDetail(status)}</p>
		{/if}

		<!--
			#414 段階2: 開始時に外した設定。**不正なものがあることは必ず分かる
			ようにする**（オーナー決定）ので、1 件でもあれば見出しと一覧を出す。
			一覧は状態の中にあるので、停止・再起動で状態が変われば入れ替わる。
		-->
		{#if exclusionsHeadline}
			<div class="collect-exclusions" role="status">
				<h3 class="collect-subheading">{exclusionsHeadline}</h3>
				<!-- 説明文は状態ごと（`collectExclusionsNote`）。全部外れた noTargets で
				「収集しています」と言わない（#422 レビュー P2）。 -->
				{#if exclusionsNote}
					<p class="note">
						{exclusionsNote.summary}
						<a href="/tags">タグ設定</a>{exclusionsNote.fix}
					</p>
				{/if}
				<table class="collect-exclusion-list">
					<thead>
						<tr>
							<th scope="col">種類</th>
							<th scope="col">名前</th>
							<th scope="col">理由</th>
						</tr>
					</thead>
					<tbody>
						{#each exclusions as exclusion (exclusion.key)}
							<tr>
								<td>{exclusionUnitLabel(exclusion.unit)}</td>
								<td>{exclusion.name}</td>
								<td>{exclusion.message}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}

		<!--
			操作は editor 以上にだけ**出す**（`disabled` にして見せない）。
			viewer に押せないボタンを並べても、できることが増えないため。
		-->
		{#if canOperate}
			<div class="collect-actions">
				{#each ACTIONS as action (action)}
					<button type="button" onclick={() => operate(action)} disabled={busy}>
						{collectActionLabel(action)}
					</button>
				{/each}
			</div>
		{:else}
			<p class="note">収集の開始・停止・再起動は、編集者以上のアカウントで行えます。</p>
		{/if}

		<!--
			`pending`（受け付けたがまだ終わっていない）・打ち切り（待つのを
			やめただけ）・エラー（未受付を含む）は**別の行・別の文言**。
			言い分けは `collectOperationDisplay` の表を参照。
		-->
		{#if opNotice}
			<p class="note operation-notice" role="status">{opNotice}</p>
		{/if}
		{#if opError}
			<p class="error">{opError}</p>
		{/if}

		<h3 class="collect-subheading">接続ごとの状態</h3>
		{#if connectionsStale}
			<p class="note poll-stale" role="status">
				{collectStaleNote('connections', lastConnectionsAt)}
			</p>
		{/if}
		{#if connections}
			<p class="note">{collectConnectionsNote(connections.state, connectionEntries.length)}</p>
			{#if connectionEntries.length > 0}
				<table class="collect-connections">
					<thead>
						<tr>
							<th scope="col">接続</th>
							<th scope="col">状態</th>
							<th scope="col">記録</th>
						</tr>
					</thead>
					<tbody>
						{#each connectionEntries as [key, value] (key)}
							{@const simulationNote = connectionSimulationNote(value)}
							<tr>
								<td class="collect-key">{key}</td>
								<td>{connectionStatusLabel(value)}</td>
								<td class:simulation-note={simulationNote !== null}>
									{simulationNote ?? '記録対象'}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		{:else}
			<p class="note">接続ごとの状態はまだ取得できていません。</p>
		{/if}

		<p class="note">
			収集の開始・停止・接続の切断などの記録は「イベント」画面で見られます。タグ設定の変更（接続のシミュレーションの切替を含む）は自動では反映されません（「収集を再起動」で反映します）。
		</p>
	{:else}
		<p class="note">
			収集はデスクトップアプリ、またはデスクトップアプリが公開しているLANサーバー経由でのみ利用できます。
		</p>
	{/if}
</section>

<style>
	.collect-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		margin-top: 0.75rem;
	}

	/*
		「今の表示が正しくない」「操作はまだ終わっていない」という注意喚起は、
		既存の `.note`（薄いグレー）より目に入る色にする。エラー（`.error`）
		ではないので `--banto-danger` は使わない（HubSection.svelte と同じ）。
	*/
	.poll-stale,
	.operation-notice {
		color: var(--banto-text);
	}

	.collect-subheading {
		margin: 1.25rem 0 0;
		font-size: 0.95rem;
	}

	.collect-key {
		font-family: var(--banto-font-mono, monospace);
	}

	/* #413: 値が記録されない接続は、エラーではないが見落とすと困るので
	既存の注意喚起（`.poll-stale` と同じ）の色で出す。 */
	.simulation-note {
		color: var(--banto-text);
		font-weight: 600;
	}

	/* #414 段階2: 外した設定。エラーではない（残りは動いている）が、見落とすと
	困るので注意喚起の色で見出しを出す。 */
	.collect-exclusions h3 {
		color: var(--banto-text);
	}

	.collect-connections,
	.collect-exclusion-list {
		margin-top: 0.5rem;
		border-collapse: collapse;
		width: 100%;
		max-width: 40rem;
	}

	.collect-connections th,
	.collect-connections td,
	.collect-exclusion-list th,
	.collect-exclusion-list td {
		text-align: left;
		padding: 0.25rem 0.5rem;
		border-bottom: 1px solid var(--banto-border);
		font-size: 0.8rem;
	}
</style>
