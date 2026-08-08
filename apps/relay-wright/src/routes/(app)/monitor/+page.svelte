<script lang="ts">
	/**
	 * タグモニタ画面 (feature/tag-monitor)。
	 *
	 * 左ペイン: PLC接続 → 収集グループ のツリー（SLMP以外の接続はグレー
	 * アウト・選択不可 — モニタはエンジンのブローカー経由で SLMP 専用）。
	 * 右ペイン: 選択グループのタグ一覧（タグ名称 / デバイス / 現在値+単位）
	 * を約1秒周期でポーリング表示する。値はバックエンドでスケーリング+
	 * 小数桁適用済みの工学値（ルールエンジンが比較する値と同じ）。
	 *
	 * 値セルをクリック（editor+ かつ手動書き込みが有効な場合）するとインライン
	 * 入力になり、Enter で即時書き込み・Esc でキャンセル。bit タグは 0/1
	 * ボタンでワンクリック書き込み。確認ダイアログは意図的に無し —
	 * 本アプリはデバッグ用途で、ユーザーが手動書き込みの安全ゲート
	 * （アーム・確認）を明示的に緩和している。ただし全書き込みはバックエンド
	 * で write_audit_log に action='manual_write' として監査される
	 * （デバッグ履歴を兼ねる）。
	 *
	 * H2（2026-08-08 オーナー決定, docs/improvement-plan.md H2 — B案）:
	 * 手動書き込みは「設定」画面のトグル（既定オフ）で明示的に有効化しない
	 * 限り拒否される。有効な間は画面上部に常時警告バナーを表示する
	 * （arm/レート制限/dry-run の対象外であることの注意）。無効時は値セルの
	 * クリックを無効化し、その理由を表示する（editor+ でも同様）。
	 *
	 * ポーリングは編集中とページ非表示中（visibilitychange）は一時停止し、
	 * ページ破棄時に必ず clearInterval する。エンジン未起動時はバナーを
	 * 表示する（モニタの読み取りはエンジンの PLC セッションを使うため —
	 * 実機 R08ENCPU は同一ポートへの SLMP 同時接続を1本しか受けず、
	 * モニタがエンジンと別に同じ接続へ独自接続を張ることはできない）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listPlcConnections,
		listCollectionGroups,
		type PlcConnection,
		type CollectionGroup
	} from '$lib/banto/tagRegistryAdmin';
	import {
		readGroup,
		writeTag,
		getMonitorConfig,
		isMonitorAvailable,
		DEMO_MODE_MESSAGE,
		ENGINE_NOT_RUNNING_MESSAGE,
		type MonitorValue,
		type MonitorConfig
	} from '$lib/banto/monitorAdmin';

	const available = isMonitorAvailable();
	const canWrite = $derived(canWriteResources(sessionStore.role));

	// H2: the settings-gate state (default assumed OFF - `false` - until the
	// real value loads, so the writable UI never flashes on before we know
	// it is actually allowed). The backend is the real authority regardless
	// (`EngineControl::monitor_write`'s gate) - this only drives what the UI
	// shows/allows clicking; a stale/unfetched value degrades safely to "not
	// writable", never to a false "writable".
	let monitorConfig = $state<MonitorConfig | null>(null);
	const manualWriteEnabled = $derived(monitorConfig?.manualWriteEnabled ?? false);
	const writeUiEnabled = $derived(canWrite && manualWriteEnabled);

	$effect(() => {
		if (!available) return;
		void (async () => {
			try {
				monitorConfig = await getMonitorConfig();
			} catch {
				// Older backend without the command, or a transient failure:
				// keep treating manual write as disabled (fail closed) rather
				// than breaking the whole page.
				monitorConfig = null;
			}
		})();
	});

	const POLL_MS = 1000;

	let connections = $state<PlcConnection[]>([]);
	let groups = $state<CollectionGroup[]>([]);
	let treeLoading = $state(true);
	let treeError = $state<string | null>(null);

	let selectedGroupId = $state<number | null>(null);
	let values = $state<MonitorValue[]>([]);
	/** 選択グループの初回読み込みが終わるまで true（「読み込み中…」表示用）。 */
	let firstLoad = $state(false);
	let engineDown = $state(false);
	let pollError = $state<string | null>(null);

	let editingTagId = $state<number | null>(null);
	let editValue = $state('');
	let editError = $state<string | null>(null);
	let writing = $state(false);
	let pageVisible = $state(true);

	const selectedGroup = $derived(groups.find((g) => g.id === selectedGroupId) ?? null);
	const groupsByConnection = $derived.by(() => {
		const map = new Map<number, CollectionGroup[]>();
		for (const group of groups) {
			const list = map.get(group.plcConnectionId) ?? [];
			list.push(group);
			map.set(group.plcConnectionId, list);
		}
		return map;
	});

	function errorMessage(err: unknown): string {
		if (isProviderError(err)) {
			// A validation error's useful text lives in the field errors (the
			// generic message is just "validation failed") - the backend puts
			// the parse/range/SJIS reason there for manual writes.
			if (err.body.kind === 'validation') {
				const messages = err.body.field_errors.map((f) => f.message).filter(Boolean);
				if (messages.length > 0) return messages.join(' / ');
			}
			return err.message;
		}
		return String(err);
	}

	// ツリー（接続・グループ一覧）の読み込み。1回だけ。
	$effect(() => {
		if (!available) {
			treeLoading = false;
			return;
		}
		void (async () => {
			try {
				const [conns, grps] = await Promise.all([listPlcConnections(), listCollectionGroups()]);
				connections = conns;
				groups = grps;
				treeError = null;
			} catch (err) {
				treeError = errorMessage(err);
			} finally {
				treeLoading = false;
			}
		})();
	});

	async function pollOnce(groupId: number): Promise<void> {
		try {
			const next = await readGroup(groupId);
			// 選択が変わった後に届いた古い応答は捨てる。
			if (groupId !== selectedGroupId) return;
			values = next;
			engineDown = false;
			pollError = null;
		} catch (err) {
			if (groupId !== selectedGroupId) return;
			const message = errorMessage(err);
			if (message.includes('エンジンが起動していません')) {
				engineDown = true;
			} else {
				pollError = message;
			}
		} finally {
			if (groupId === selectedGroupId) firstLoad = false;
		}
	}

	// 選択グループの約1秒ポーリング。編集中・ページ非表示中はスキップし、
	// 選択変更/ページ破棄でクリーンアップする。
	$effect(() => {
		if (!available || selectedGroupId === null) return;
		const groupId = selectedGroupId;
		let cancelled = false;

		const tick = () => {
			// editingTagId / pageVisible はコールバック内での読み取りなので
			// リアクティブ購読にならない（編集開始のたびに effect が再実行
			// されてポーリングがリセットされることはない）。
			if (cancelled || !pageVisible || editingTagId !== null) return;
			void pollOnce(groupId);
		};
		tick();
		const interval = setInterval(tick, POLL_MS);
		const onVisibility = () => {
			pageVisible = !document.hidden;
		};
		document.addEventListener('visibilitychange', onVisibility);
		return () => {
			cancelled = true;
			clearInterval(interval);
			document.removeEventListener('visibilitychange', onVisibility);
		};
	});

	function selectGroup(group: CollectionGroup): void {
		if (selectedGroupId === group.id) return;
		selectedGroupId = group.id;
		values = [];
		firstLoad = true;
		pollError = null;
		cancelEdit();
	}

	function startEdit(entry: MonitorValue): void {
		if (!writeUiEnabled || !available) return;
		editingTagId = entry.tagId;
		editValue = entry.value === null ? '' : String(entry.value);
		editError = null;
	}

	function cancelEdit(): void {
		editingTagId = null;
		editValue = '';
		editError = null;
	}

	async function commitWrite(tagId: number, value: string): Promise<void> {
		writing = true;
		try {
			await writeTag(tagId, value);
			toastStore.push('success', '書き込みました');
			cancelEdit();
			if (selectedGroupId !== null) await pollOnce(selectedGroupId);
		} catch (err) {
			editError = errorMessage(err);
		} finally {
			writing = false;
		}
	}

	function onEditKeydown(event: KeyboardEvent, tagId: number): void {
		if (event.key === 'Enter') {
			event.preventDefault();
			void commitWrite(tagId, editValue);
		} else if (event.key === 'Escape') {
			event.preventDefault();
			cancelEdit();
		}
	}

	/** インライン入力を開いた瞬間にフォーカス+全選択（すぐ打てるように）。 */
	function focusOnMount(node: HTMLInputElement) {
		node.focus();
		node.select();
	}

	function displayValue(entry: MonitorValue): string {
		if (entry.quality !== 'good' || entry.value === null) return '--';
		return String(entry.value);
	}

	/**
	 * Tooltip for a non-writable value cell: a read error takes priority
	 * (unchanged behavior); otherwise, for an `editor`+ who COULD write if the
	 * H2 gate were on, explain why the cell isn't clickable (viewers get no
	 * tooltip here, same as before H2 - the reason is role, already explained
	 * by the note above the table).
	 */
	function readOnlyTitle(entry: MonitorValue): string {
		if (entry.quality === 'bad') return entry.error ?? '読み取りエラー';
		if (canWrite && !manualWriteEnabled) return '手動書き込みは設定で無効です';
		return '';
	}
</script>

<div class="page">
	<h2>モニタ</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードには PLC
			セッションが無いため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		{#if engineDown}
			<p class="banner">
				{ENGINE_NOT_RUNNING_MESSAGE}
				— モニタは読み取りにエンジンのPLCセッションを使用します（PLCは同時接続を1本しか受けないため、モニタが独自に接続することはありません）。エンジン制御画面から再構築するか、アプリを再起動してください。
			</p>
		{/if}

		{#if manualWriteEnabled}
			<p class="banner">
				⚠ 手動書き込みは安全ゲート対象外です — arm / レート制限 / dry-run
				の対象外で、disarm中でも物理書き込みが行われます。無効化するには「設定」画面へ。
			</p>
		{/if}

		<p class="note">
			接続→収集グループを選ぶと、そのグループのタグの現在値を約1秒周期で表示します。
			{#if writeUiEnabled}
				値セルをクリックすると即時書き込みできます（確認なし・全書き込みは監査ログに記録されます）。
			{:else if canWrite}
				手動書き込みは設定で無効です（設定画面から有効化できます）。
			{:else}
				書き込みには編集者以上の権限が必要です（閲覧のみ）。
			{/if}
		</p>

		<div class="layout">
			<nav class="tree" aria-label="接続と収集グループ">
				{#if treeLoading}
					<p class="muted">読み込み中…</p>
				{:else if treeError}
					<p class="error-text">{treeError}</p>
				{:else if connections.length === 0}
					<p class="muted">PLC接続が登録されていません。</p>
				{:else}
					<ul class="conn-list">
						{#each connections as conn (conn.id)}
							{@const slmp = conn.protocol === 'slmp'}
							<li class="conn" class:disabled={!slmp}>
								<div class="conn-name">
									<span class="conn-icon">🔌</span>
									<span>{conn.name}</span>
									{#if !slmp}
										<span class="conn-note">SLMP以外（モニタ対象外）</span>
									{/if}
								</div>
								<ul class="group-list">
									{#each groupsByConnection.get(conn.id) ?? [] as group (group.id)}
										<li>
											<button
												type="button"
												class="group"
												class:selected={selectedGroupId === group.id}
												disabled={!slmp}
												onclick={() => selectGroup(group)}
											>
												{group.name}
											</button>
										</li>
									{:else}
										<li class="muted small">収集グループなし</li>
									{/each}
								</ul>
							</li>
						{/each}
					</ul>
				{/if}
			</nav>

			<section class="values">
				{#if selectedGroup === null}
					<p class="muted">左のツリーから収集グループを選択してください。</p>
				{:else}
					<div class="values-header">
						<h3>{selectedGroup.name}</h3>
						<span class="muted small">約1秒周期で更新</span>
					</div>

					{#if pollError}
						<p class="error-text">{pollError}</p>
					{/if}

					{#if firstLoad}
						<p class="muted">読み込み中…</p>
					{:else if values.length === 0}
						<p class="muted">このグループに有効なタグがありません。</p>
					{:else}
						<table>
							<thead>
								<tr>
									<th>タグ名称</th>
									<th>デバイス</th>
									<th class="value-col">現在値</th>
								</tr>
							</thead>
							<tbody>
								{#each values as entry (entry.tagId)}
									<tr>
										<td>{entry.tagName}</td>
										<td class="mono">{entry.address}</td>
										<td class="value-col">
											{#if editingTagId === entry.tagId}
												{#if entry.dataType === 'bit'}
													<span class="bit-actions">
														<button
															type="button"
															class="bit"
															disabled={writing}
															onclick={() => void commitWrite(entry.tagId, '0')}
														>
															0
														</button>
														<button
															type="button"
															class="bit"
															disabled={writing}
															onclick={() => void commitWrite(entry.tagId, '1')}
														>
															1
														</button>
														<button
															type="button"
															class="cancel"
															disabled={writing}
															onclick={cancelEdit}
														>
															キャンセル
														</button>
													</span>
												{:else}
													<input
														class="edit"
														type="text"
														bind:value={editValue}
														disabled={writing}
														use:focusOnMount
														onkeydown={(event) => onEditKeydown(event, entry.tagId)}
													/>
													<span class="edit-hint">Enterで書き込み / Escで取消</span>
												{/if}
												{#if editError}
													<span class="error-text small">{editError}</span>
												{/if}
											{:else if writeUiEnabled}
												<button
													type="button"
													class="value-cell writable"
													title={entry.quality === 'bad'
														? (entry.error ?? '読み取りエラー')
														: 'クリックで書き込み'}
													onclick={() => startEdit(entry)}
												>
													<span class="value" class:bad={entry.quality === 'bad'}>
														{displayValue(entry)}
													</span>
													{#if entry.unit && entry.quality === 'good'}
														<span class="unit">{entry.unit}</span>
													{/if}
													{#if entry.quality === 'bad'}
														<span class="badge-bad">BAD</span>
													{/if}
												</button>
											{:else}
												<span class="value-cell" title={readOnlyTitle(entry)}>
													<span class="value" class:bad={entry.quality === 'bad'}>
														{displayValue(entry)}
													</span>
													{#if entry.unit && entry.quality === 'good'}
														<span class="unit">{entry.unit}</span>
													{/if}
													{#if entry.quality === 'bad'}
														<span class="badge-bad">BAD</span>
													{/if}
												</span>
											{/if}
										</td>
									</tr>
								{/each}
							</tbody>
						</table>
					{/if}
				{/if}
			</section>
		</div>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	h3 {
		margin: 0;
		font-size: 0.95rem;
	}

	.note {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.muted {
		color: var(--banto-text-muted);
		font-size: 0.85rem;
		margin: 0;
	}

	.small {
		font-size: 0.75rem;
	}

	.mono {
		font-family: var(--banto-font-mono, ui-monospace, monospace);
	}

	.error-text {
		color: var(--banto-danger);
		font-size: 0.8rem;
		margin: 0;
	}

	/* エンジン未起動の通知バナー（engine 画面の .banner と同じ扱い）。 */
	.banner {
		margin: 0;
		padding: 0.6rem 0.85rem;
		font-size: 0.85rem;
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
		border: 1px solid var(--banto-danger);
		border-radius: var(--banto-radius);
	}

	.layout {
		display: grid;
		grid-template-columns: 280px 1fr;
		gap: 1rem;
		align-items: start;
	}

	.tree,
	.values {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 0.85rem 1rem;
	}

	.conn-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
	}

	.conn.disabled {
		opacity: 0.55;
	}

	.conn-name {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-weight: 600;
		font-size: 0.85rem;
	}

	.conn-note {
		font-size: 0.7rem;
		font-weight: 400;
		color: var(--banto-text-muted);
	}

	.group-list {
		list-style: none;
		margin: 0.25rem 0 0;
		padding: 0 0 0 1.35rem;
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}

	button.group {
		display: block;
		width: 100%;
		text-align: left;
		padding: 0.35rem 0.55rem;
		border: none;
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-text);
		font-size: 0.85rem;
		cursor: pointer;
	}

	button.group:hover:not(:disabled) {
		background: var(--banto-bg);
	}

	button.group.selected {
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
		color: var(--banto-primary);
		font-weight: 600;
	}

	button.group:disabled {
		cursor: not-allowed;
	}

	.values-header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.75rem;
		margin-bottom: 0.5rem;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}

	th,
	td {
		text-align: left;
		padding: 0.45rem 0.6rem;
		border-bottom: 1px solid var(--banto-border);
	}

	th {
		color: var(--banto-text-muted);
		font-weight: 600;
		font-size: 0.75rem;
	}

	.value-col {
		width: 40%;
	}

	.value-cell {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		min-height: 1.6rem;
	}

	button.value-cell {
		border: 1px solid transparent;
		border-radius: var(--banto-radius);
		background: transparent;
		color: inherit;
		font: inherit;
		padding: 0.15rem 0.45rem;
		cursor: pointer;
	}

	button.value-cell.writable:hover {
		border-color: var(--banto-border);
		background: var(--banto-bg);
	}

	.value {
		font-family: var(--banto-font-mono, ui-monospace, monospace);
		font-size: 0.95rem;
	}

	.value.bad {
		color: var(--banto-text-muted);
	}

	.unit {
		color: var(--banto-text-muted);
		font-size: 0.75rem;
	}

	.badge-bad {
		font-size: 0.65rem;
		font-weight: 700;
		padding: 0.05rem 0.35rem;
		border-radius: var(--banto-radius);
		color: var(--banto-danger);
		border: 1px solid var(--banto-danger);
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}

	input.edit {
		width: 9rem;
		padding: 0.25rem 0.45rem;
		border: 1px solid var(--banto-primary);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-family: var(--banto-font-mono, ui-monospace, monospace);
		font-size: 0.9rem;
	}

	.edit-hint {
		margin-left: 0.4rem;
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	.bit-actions {
		display: inline-flex;
		gap: 0.35rem;
		align-items: center;
	}

	button.bit {
		min-width: 2.2rem;
		padding: 0.25rem 0.55rem;
		border: 1px solid var(--banto-primary);
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-primary);
		font-weight: 700;
		cursor: pointer;
	}

	button.bit:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 12%, transparent);
	}

	button.cancel {
		padding: 0.25rem 0.55rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-text-muted);
		font-size: 0.75rem;
		cursor: pointer;
	}

	button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.error-text.small {
		display: block;
		margin-top: 0.25rem;
	}

	@media (max-width: 900px) {
		.layout {
			grid-template-columns: 1fr;
		}
	}
</style>
