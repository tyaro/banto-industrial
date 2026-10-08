<script lang="ts">
	/**
	 * グループ設定画面（#393 = #524 の段階 1、recorder-requirements.md §6 の 4
	 * 「グループ設定（ペン割当・表示種別・しきい値）」）。
	 *
	 * 表示グループは**表示の単位**（収集グループとは別物）。1 グループに最大
	 * 8 ペン（タグの割り当て）と、トレンド / デジタル / バー / 計器の表示種別を
	 * 持つ。監視画面での描画は R1-D（段階 2）で、この画面は定義の編集だけ。
	 *
	 * - **しきい値は読み取り専用**で、各ペンのタグの値（LL / L / H / HH）を
	 *   出すだけ。しきい値はタグ定義の属性で、保存はタグ定義の経路（タグ設定）で
	 *   行う（§3.7.1・§3.7.6、2026-10-08 オーナー決定）。
	 * - 検証はサーバー側の 1 関数（`validate_display_group`）。画面は上限・
	 *   選択肢を「選べるものだけ出す」ことで守り、誤りはサーバーの
	 *   `field_errors` を欄ごとに出す（`groupsPageLogic.ts` の `sortFieldErrors`。
	 *   どの欄にも出せないものはトーストへ - 黙って捨てない）。
	 * - 更新は楽観ロック（`expectedRevision`）。先に別の人が更新していたら
	 *   保存を拒否し、入力は残したまま「最新の内容を読み込む」を出す。
	 * - 未保存の入力の確認（#508 の `guardUnsavedChanges`）: 編集中の下書きが
	 *   読み込み時と違えば、画面の移動・別グループの選択の前に確かめる。
	 * - viewer は閲覧のみ（一覧と各グループの中身は見えるが、入力欄は無効）。
	 */
	import { UnsavedChangesNotice } from '@banto/forms';
	import { isProviderError } from '@banto/admin-core';
	import {
		guardUnsavedChanges,
		UNSAVED_CONFIRM_LEAVE,
		UNSAVED_DISCARD,
		UNSAVED_NOTICE
	} from '#lib/unsavedChanges.js';
	import { toastStore } from '#lib/toast.svelte.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { canWriteResources } from '#lib/permissions.js';
	import {
		listTags,
		listCollectionGroups,
		isTagRegistryAvailable,
		DEMO_MODE_MESSAGE,
		type Tag,
		type CollectionGroup
	} from '#lib/banto/tagRegistryAdmin.js';
	import { listTagThresholds, type TagThresholds } from '#lib/banto/tagThresholdsAdmin.js';
	import {
		listDisplayGroups,
		createDisplayGroup,
		updateDisplayGroup,
		deleteDisplayGroup,
		reorderDisplayGroups,
		type DisplayGroup,
		type DisplayKind
	} from '#lib/banto/displayGroupsAdmin.js';
	import {
		COLOR_SLOTS,
		KIND_OPTIONS,
		MAX_DISPLAY_GROUPS,
		MAX_PENS,
		TIME_WINDOW_OPTIONS,
		addPen,
		canAddPen,
		colorSlotVar,
		defaultColorSlot,
		draftFromGroup,
		draftProblems,
		draftToInput,
		effectiveColorSlot,
		emptyDraft,
		emptyFieldErrors,
		isDraftDirty,
		kindLabel,
		movePen,
		movedGroupIds,
		removePen,
		reloadLatestGroup,
		sortFieldErrors,
		tagsUsedByOtherPens,
		penThresholdText,
		type GroupDraft
	} from './groupsPageLogic';

	const available = isTagRegistryAvailable();
	const canWrite = $derived(canWriteResources(sessionStore.role));

	// --- 一覧（「読めていない」と「0 件」を分ける - チェックリスト §5） --------

	let groups = $state<DisplayGroup[] | null>(null);
	let groupsError = $state<string | null>(null);
	let groupsLoading = $state(false);

	let tags = $state<Tag[] | null>(null);
	let tagsError = $state<string | null>(null);
	let collectionGroups = $state<CollectionGroup[]>([]);

	function errorMessage(err: unknown): string {
		if (isProviderError(err)) {
			return err.body.kind === 'validation'
				? err.body.field_errors.map((fe) => fe.message).join(' / ')
				: err.message;
		}
		return String(err);
	}

	async function reloadGroups(): Promise<void> {
		groupsLoading = true;
		try {
			groups = await listDisplayGroups();
			groupsError = null;
		} catch (err) {
			groupsError = errorMessage(err);
		} finally {
			groupsLoading = false;
		}
	}

	async function reloadTags(): Promise<void> {
		void reloadThresholds();
		try {
			const [tagRows, groupRows] = await Promise.all([listTags(), listCollectionGroups()]);
			tags = tagRows;
			collectionGroups = groupRows;
			tagsError = null;
		} catch (err) {
			tagsError = errorMessage(err);
		}
	}

	// #532: しきい値は記録計の側の設定（`/api/tag-thresholds`）から読む（読み取り
	// 専用の表示）。タグの一覧とは別に読み、読めなかったことは「しきい値なし」と
	// 区別して出す（`penThresholdText`）。読み直しに失敗したら前の値は捨てる（古い
	// しきい値を今の設定のように見せない。監視画面の `applyTagMetaLoad` と同じ）。
	let thresholdsByTag = $state<Map<number, TagThresholds> | null>(null);
	let thresholdsError = $state<string | null>(null);

	async function reloadThresholds(): Promise<void> {
		try {
			const rows = await listTagThresholds();
			thresholdsByTag = new Map(rows.map((row) => [row.tagId, row]));
			thresholdsError = null;
		} catch (err) {
			thresholdsByTag = null;
			thresholdsError = errorMessage(err);
		}
	}

	const tagsById = $derived(new Map((tags ?? []).map((tag) => [tag.id, tag])));

	/** タグの選択肢を収集グループごとにまとめる（名前順）。 */
	const tagOptionGroups = $derived.by(() => {
		const byGroup = new Map<number, Tag[]>();
		for (const tag of tags ?? []) {
			const list = byGroup.get(tag.collectionGroupId) ?? [];
			list.push(tag);
			byGroup.set(tag.collectionGroupId, list);
		}
		return [...byGroup.entries()].map(([groupId, list]) => ({
			label:
				collectionGroups.find((group) => group.id === groupId)?.name ?? `収集グループ ${groupId}`,
			tags: [...list].sort((a, b) => a.name.localeCompare(b.name, 'ja'))
		}));
	});

	function tagLabel(tagId: number): string {
		return tagsById.get(tagId)?.name ?? `（見つからないタグ ID ${tagId}）`;
	}

	// --- 編集 -----------------------------------------------------------------

	/** `'new'` = 新規作成中、数値 = 編集中のグループの ID、`null` = 未選択。 */
	let selected = $state<'new' | number | null>(null);
	let editingRevision = $state<number | undefined>(undefined);
	let draft = $state<GroupDraft>(emptyDraft());
	let baseline = $state<GroupDraft>(emptyDraft());
	let fieldErrors = $state(emptyFieldErrors());
	let saving = $state(false);
	let deleting = $state(false);
	let reordering = $state(false);
	/** 「最新の内容を読み込む」の待ち（P2-2: 待っている間は編集・選択の操作を止める）。 */
	let reloadingLatest = $state(false);
	/** 「最新の内容を読み込む」が失敗した理由（P2-5: 入力は残したまま出す）。 */
	let latestReloadError = $state<string | null>(null);
	/**
	 * 選択・編集の世代。開き直す・新規・閉じる・取り消すのたびに進める。遅れて届いた
	 * 「最新の内容」の応答は、世代が変わっていたら捨てる（P2-2 の二重の守り）。
	 */
	let editGeneration = 0;

	const dirty = $derived(selected !== null && isDraftDirty(draft, baseline));
	const busy = $derived(saving || deleting || reordering || reloadingLatest);
	const selectedGroup = $derived(
		typeof selected === 'number' ? (groups ?? []).find((group) => group.id === selected) : undefined
	);
	const atGroupLimit = $derived((groups?.length ?? 0) >= MAX_DISPLAY_GROUPS);

	guardUnsavedChanges({
		isDirty: () => dirty,
		isSaving: () => saving || deleting
	});

	function cloneDraft(value: GroupDraft): GroupDraft {
		return JSON.parse(JSON.stringify(value)) as GroupDraft;
	}

	/** 未保存の入力を捨ててよいか（無ければ聞かない）。 */
	function confirmDiscard(): boolean {
		return !dirty || window.confirm(UNSAVED_CONFIRM_LEAVE);
	}

	function openGroup(group: DisplayGroup): void {
		editGeneration += 1;
		latestReloadError = null;
		selected = group.id;
		editingRevision = group.revision;
		baseline = draftFromGroup(group);
		draft = cloneDraft(baseline);
		fieldErrors = emptyFieldErrors();
	}

	function selectGroup(group: DisplayGroup): void {
		if (busy || selected === group.id) return;
		if (!confirmDiscard()) return;
		openGroup(group);
	}

	function startNew(): void {
		if (busy || !confirmDiscard()) return;
		editGeneration += 1;
		latestReloadError = null;
		selected = 'new';
		editingRevision = undefined;
		baseline = emptyDraft();
		draft = emptyDraft();
		fieldErrors = emptyFieldErrors();
	}

	function closeEditor(): void {
		if (busy || !confirmDiscard()) return;
		editGeneration += 1;
		latestReloadError = null;
		selected = null;
		fieldErrors = emptyFieldErrors();
	}

	function discardChanges(): void {
		editGeneration += 1;
		draft = cloneDraft(baseline);
		fieldErrors = emptyFieldErrors();
	}

	/**
	 * 版の食い違いのあと: 入力を捨てて、最新の内容を開き直す。判断は
	 * `reloadLatestGroup`（`groupsPageLogic.ts`）: 待っている間に選択・編集が
	 * 変わったら何もしない（P2-2）、読めなかったら古い行で開き直さず入力を残して
	 * 理由を出す（P2-5）。待っている間は `busy` で編集・選択の操作を止める。
	 */
	async function reloadLatest(): Promise<void> {
		if (busy || typeof selected !== 'number') return;
		const id = selected;
		const generation = editGeneration;
		reloadingLatest = true;
		latestReloadError = null;
		try {
			const outcome = await reloadLatestGroup({
				id,
				load: listDisplayGroups,
				isCurrent: () => editGeneration === generation && selected === id,
				describeError: errorMessage
			});
			switch (outcome.kind) {
				case 'open':
					groups = outcome.groups;
					groupsError = null;
					openGroup(outcome.group);
					break;
				case 'deleted':
					groups = outcome.groups;
					groupsError = null;
					editGeneration += 1;
					selected = null;
					toastStore.push('error', 'このグループは削除されています');
					break;
				case 'failed':
					latestReloadError = outcome.message;
					break;
				case 'stale':
					break;
			}
		} finally {
			reloadingLatest = false;
		}
	}

	function setKind(kind: DisplayKind): void {
		draft.kind = kind;
	}

	function setPenTag(index: number, value: string): void {
		draft.pens[index].tagId = value === '' ? null : Number(value);
	}

	function setPenColor(index: number, value: string): void {
		draft.pens[index].colorSlot = value === '' ? null : Number(value);
	}

	async function save(): Promise<void> {
		if (busy || selected === null) return;
		const problems = draftProblems(draft);
		if (problems.length > 0) {
			for (const problem of problems) toastStore.push('error', problem);
			return;
		}
		saving = true;
		fieldErrors = emptyFieldErrors();
		latestReloadError = null;
		const target = selected;
		try {
			const saved =
				target === 'new'
					? await createDisplayGroup(draftToInput(draft))
					: await updateDisplayGroup(target, draftToInput(draft, editingRevision));
			toastStore.push(
				'success',
				target === 'new' ? `${saved.name} を作成しました` : `${saved.name} を更新しました`
			);
			await reloadGroups();
			openGroup(saved);
		} catch (err) {
			if (isProviderError(err) && err.body.kind === 'validation') {
				fieldErrors = sortFieldErrors(err.body.field_errors);
				for (const message of fieldErrors.other) toastStore.push('error', message);
				if (fieldErrors.revision.length > 0) void reloadGroups();
			} else {
				toastStore.push('error', errorMessage(err));
			}
		} finally {
			saving = false;
		}
	}

	async function remove(): Promise<void> {
		if (busy || typeof selected !== 'number' || !selectedGroup) return;
		if (!window.confirm(`${selectedGroup.name} を削除しますか？`)) return;
		deleting = true;
		const name = selectedGroup.name;
		try {
			await deleteDisplayGroup(selected);
			toastStore.push('success', `${name} を削除しました`);
			selected = null;
			await reloadGroups();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			deleting = false;
		}
	}

	async function moveGroup(id: number, delta: -1 | 1): Promise<void> {
		if (busy || !groups) return;
		const ids = movedGroupIds(groups, id, delta);
		if (!ids) return;
		reordering = true;
		try {
			groups = await reorderDisplayGroups(ids);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
			await reloadGroups();
		} finally {
			reordering = false;
		}
	}

	$effect(() => {
		if (!available) return;
		void reloadGroups();
		void reloadTags();
	});
</script>

<div class="page">
	<h2>グループ設定</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはDBが無いため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<p class="note">
			表示グループは監視画面の表示の単位です（収集グループとは別物）。1 グループに最大 {MAX_PENS} 本のペン（タグ）と表示種別を設定します。グループは最大
			{MAX_DISPLAY_GROUPS} 個です。しきい値はタグの設定で、ここでは確認だけできます。
		</p>

		<section class="groups-section" aria-label="表示グループの一覧">
			<div class="section-head">
				<h3>表示グループ</h3>
				{#if canWrite}
					<button
						type="button"
						class="primary"
						onclick={startNew}
						disabled={busy || atGroupLimit || groups === null}
					>
						新規グループ
					</button>
				{/if}
			</div>
			{#if canWrite && atGroupLimit}
				<p class="note">
					表示グループは {MAX_DISPLAY_GROUPS} 個までです。新しく作るには不要なグループを削除してください。
				</p>
			{/if}

			{#if groups === null && groupsError === null}
				<p class="loading">読み込み中…</p>
			{:else if groups === null}
				<p class="load-error" role="alert">
					表示グループの一覧を読み込めませんでした（{groupsError}）。登録が0件という意味ではありません。
					<button type="button" onclick={() => void reloadGroups()} disabled={groupsLoading}
						>再試行</button
					>
				</p>
			{:else}
				{#if groupsError !== null}
					<p class="load-error" role="alert">
						一覧を更新できませんでした（{groupsError}）。表示は最後に読み込めた内容です。
						<button type="button" onclick={() => void reloadGroups()} disabled={groupsLoading}
							>再試行</button
						>
					</p>
				{/if}
				{#if groups.length === 0}
					<p class="note">表示グループはまだありません。</p>
				{:else}
					<ul class="group-list">
						{#each groups as group, index (group.id)}
							<li class:selected={selected === group.id}>
								<button
									type="button"
									class="group-open"
									onclick={() => selectGroup(group)}
									disabled={busy}
								>
									<span class="group-name">{group.name}</span>
									<span class="group-meta"
										>{kindLabel(group.kind)}・ペン {group.pens.length} 本</span
									>
								</button>
								{#if canWrite}
									<button
										type="button"
										class="icon"
										aria-label={`${group.name} を上へ`}
										onclick={() => void moveGroup(group.id, -1)}
										disabled={busy || index === 0}
									>
										↑
									</button>
									<button
										type="button"
										class="icon"
										aria-label={`${group.name} を下へ`}
										onclick={() => void moveGroup(group.id, 1)}
										disabled={busy || index === groups.length - 1}
									>
										↓
									</button>
								{/if}
							</li>
						{/each}
					</ul>
				{/if}
			{/if}
		</section>

		{#if selected !== null}
			<section class="editor-section" aria-label="表示グループの編集">
				<div class="section-head">
					<h3>
						{selected === 'new'
							? '新しい表示グループ'
							: canWrite
								? `${selectedGroup?.name ?? ''} を編集`
								: (selectedGroup?.name ?? '')}
					</h3>
					<UnsavedChangesNotice pending={dirty} label={UNSAVED_NOTICE} />
				</div>

				{#if fieldErrors.revision.length > 0}
					<p class="load-error" role="alert">
						{fieldErrors.revision.join(' / ')}
						<button type="button" onclick={() => void reloadLatest()} disabled={busy}>
							{reloadingLatest ? '読み込み中…' : '最新の内容を読み込む（入力を破棄）'}
						</button>
					</p>
					{#if latestReloadError !== null}
						<p class="load-error" role="alert">
							最新の内容を読み込めませんでした（{latestReloadError}）。入力はそのまま残っています。
						</p>
					{/if}
				{/if}

				<fieldset class="editor" disabled={!canWrite || busy}>
					<label class="field">
						<span>名前</span>
						<input type="text" maxlength="100" bind:value={draft.name} />
						{#each fieldErrors.name as message, i (i)}
							<span class="field-error">{message}</span>
						{/each}
					</label>

					<label class="field">
						<span>表示種別</span>
						<select
							value={draft.kind}
							onchange={(e) => setKind(e.currentTarget.value as DisplayKind)}
						>
							{#each KIND_OPTIONS as option (option.value)}
								<option value={option.value}>{option.label}</option>
							{/each}
						</select>
						{#each fieldErrors.kind as message, i (i)}
							<span class="field-error">{message}</span>
						{/each}
					</label>

					{#if draft.kind === 'trend'}
						<label class="field">
							<span>既定の時間窓</span>
							<select
								value={String(draft.timeWindowSec)}
								onchange={(e) => (draft.timeWindowSec = Number(e.currentTarget.value))}
							>
								{#each TIME_WINDOW_OPTIONS as option (option.value)}
									<option value={String(option.value)}>{option.label}</option>
								{/each}
							</select>
							{#each fieldErrors.timeWindowSec as message, i (i)}
								<span class="field-error">{message}</span>
							{/each}
						</label>
					{/if}

					<div class="pens">
						<div class="pens-head">
							<span class="pens-title">ペン（{draft.pens.length} / {MAX_PENS}）</span>
							{#if canWrite}
								<button
									type="button"
									onclick={() => (draft = addPen(draft))}
									disabled={!canAddPen(draft) || tags === null}
								>
									ペンを追加
								</button>
							{/if}
						</div>
						{#if tagsError !== null}
							<p class="load-error" role="alert">
								タグの一覧を読み込めませんでした（{tagsError}）。
								<button type="button" onclick={() => void reloadTags()}>再試行</button>
							</p>
						{/if}
						{#each fieldErrors.pens as message, i (i)}
							<p class="field-error">{message}</p>
						{/each}
						{#if draft.pens.length === 0}
							<p class="note">ペンがありません。「ペンを追加」でタグを割り当てます。</p>
						{:else}
							<ol class="pen-list">
								{#each draft.pens as pen, index (index)}
									{@const used = tagsUsedByOtherPens(draft, index)}
									{@const slot = effectiveColorSlot(pen, index)}
									<li class="pen">
										<span class="swatch" style:background={colorSlotVar(slot)} aria-hidden="true"
										></span>
										<label class="pen-field">
											<span>ペン {index + 1} のタグ</span>
											<select
												value={pen.tagId === null ? '' : String(pen.tagId)}
												onchange={(e) => setPenTag(index, e.currentTarget.value)}
											>
												<option value="">タグを選択</option>
												{#if pen.tagId !== null && !tagsById.has(pen.tagId)}
													<option value={String(pen.tagId)}>{tagLabel(pen.tagId)}</option>
												{/if}
												{#each tagOptionGroups as optionGroup (optionGroup.label)}
													<optgroup label={optionGroup.label}>
														{#each optionGroup.tags as tag (tag.id)}
															<option value={String(tag.id)} disabled={used.has(tag.id)}
																>{tag.name}{tag.unit ? `（${tag.unit}）` : ''}</option
															>
														{/each}
													</optgroup>
												{/each}
											</select>
										</label>
										<label class="pen-field color">
											<span>ペン {index + 1} の色</span>
											<select
												value={pen.colorSlot === null ? '' : String(pen.colorSlot)}
												onchange={(e) => setPenColor(index, e.currentTarget.value)}
											>
												<option value="">既定（色 {defaultColorSlot(index)}）</option>
												{#each Array.from({ length: COLOR_SLOTS }, (_, i) => i + 1) as colorSlot (colorSlot)}
													<option value={String(colorSlot)}>色 {colorSlot}</option>
												{/each}
											</select>
										</label>
										<span
											class="thresholds"
											title="しきい値はタグ設定画面の「しきい値（記録計の設定）」で変更します"
										>
											しきい値: {penThresholdText(
												pen.tagId,
												pen.tagId !== null && tagsById.has(pen.tagId),
												thresholdsByTag,
												thresholdsError !== null
											)}
										</span>
										{#if canWrite}
											<span class="pen-actions">
												<button
													type="button"
													class="icon"
													aria-label={`ペン ${index + 1} を上へ`}
													onclick={() => (draft = movePen(draft, index, -1))}
													disabled={index === 0}
												>
													↑
												</button>
												<button
													type="button"
													class="icon"
													aria-label={`ペン ${index + 1} を下へ`}
													onclick={() => (draft = movePen(draft, index, 1))}
													disabled={index === draft.pens.length - 1}
												>
													↓
												</button>
												<button
													type="button"
													class="icon"
													aria-label={`ペン ${index + 1} を外す`}
													onclick={() => (draft = removePen(draft, index))}
												>
													×
												</button>
											</span>
										{/if}
										{#each fieldErrors.penRows[index] ?? [] as message, i (i)}
											<span class="field-error pen-error">{message}</span>
										{/each}
									</li>
								{/each}
							</ol>
						{/if}
					</div>
				</fieldset>

				<div class="actions">
					{#if canWrite}
						<button type="button" class="primary" onclick={() => void save()} disabled={busy}>
							{selected === 'new' ? '作成' : '保存'}
						</button>
						<button type="button" onclick={discardChanges} disabled={busy || !dirty}>
							{UNSAVED_DISCARD}
						</button>
						{#if typeof selected === 'number'}
							<button type="button" class="danger" onclick={() => void remove()} disabled={busy}>
								削除
							</button>
						{/if}
					{:else}
						<p class="note">閲覧のみ（編集には編集者以上の権限が必要です）。</p>
					{/if}
					<button type="button" onclick={closeEditor} disabled={busy}>閉じる</button>
				</div>
			</section>
		{/if}
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		max-width: 860px;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	h3 {
		margin: 0;
		font-size: 0.95rem;
	}

	.groups-section,
	.editor-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	.section-head {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	.section-head h3 {
		margin-right: auto;
	}

	.note {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.load-error {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	button {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.75rem;
		background: transparent;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		cursor: pointer;
	}

	button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	button.primary {
		background: var(--banto-primary-solid);
		border-color: var(--banto-primary-solid);
		color: var(--banto-on-solid);
		font-weight: 600;
	}

	button.danger {
		border-color: var(--banto-danger);
		color: var(--banto-danger);
		font-weight: 600;
	}

	button.icon {
		width: var(--banto-control-height);
		padding: 0;
	}

	.group-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.group-list li {
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}

	.group-open {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		text-align: left;
	}

	.group-list li.selected .group-open {
		border-color: var(--banto-primary);
	}

	.group-meta {
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.editor {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		border: none;
		margin: 0;
		padding: 0;
	}

	.field,
	.pen-field {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.85rem;
	}

	.field input,
	.field select,
	.pen-field select {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.5rem;
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		max-width: 24rem;
	}

	.field-error {
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	.pens {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.pens-head {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}

	.pens-title {
		font-size: 0.85rem;
		font-weight: 600;
	}

	.pen-list {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.pen {
		display: flex;
		align-items: flex-end;
		gap: 0.75rem;
		flex-wrap: wrap;
		padding: 0.5rem 0;
		border-top: 1px solid var(--banto-border);
	}

	.swatch {
		width: 1rem;
		height: 1rem;
		border-radius: 50%;
		align-self: center;
		flex: none;
	}

	.pen-field.color select {
		max-width: 10rem;
	}

	.thresholds {
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		align-self: center;
	}

	.pen-actions {
		display: flex;
		gap: 0.25rem;
		margin-left: auto;
	}

	.pen-error {
		flex-basis: 100%;
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}
</style>
