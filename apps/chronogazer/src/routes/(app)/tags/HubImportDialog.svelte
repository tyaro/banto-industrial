<script lang="ts">
	/**
	 * 「Hub から追加」のダイアログ（#383 段階3 P4、2026-10-11 オーナー決定）。
	 *
	 * Hub のカタログ（`getHubCatalog` = `GET /api/hub/catalog` / `hub_catalog`。
	 * editor が読める）を一覧し、検索・複数選択して、Hub 接続の下の収集グループ
	 * （既存か、名前と周期を入れて新しく作る）にタグとして登録する
	 * （`registerTagsFromHub`）。判断はすべて `#lib/banto/hubRegistration.ts` の
	 * 純関数で、ここはそれを出すだけ。
	 *
	 * - **数値と bit だけ**選べる。文字列・構造体は理由付きで選べない。
	 * - **登録済み**の外部名は「登録済み」と出して選べない。
	 * - P4 より前の Hub 接続の画面で選んでいたタグは、選べるものだけ**1 度だけ**
	 *   事前にチェックし、注記を出す。「以前の選択を使わない」で消せる（登録が
	 *   成功してもサーバーが消す）。
	 * - 名前の衝突などは**その行**に出す（黙って改名しない）。1 行でもエラーが
	 *   あれば何も登録されない（サーバーが全部か無し）。
	 *
	 * 層の約束: 中央モーダル（`#lib/components/Modal.svelte`。banto-hub の写し）
	 * なので、Esc・オーバーレイ・× の 3 経路が同じ閉じる処理を通り、フォーカスは
	 * 中に閉じ込め、閉じたら開いた元へ戻る。未保存（チェックを変えた・新しい
	 * グループの名前を入れた）の間は Esc とオーバーレイでは閉じず、× は確認を
	 * 出す。画面の移動も `guardUnsavedChanges` で確認する。
	 */
	import Modal from '#lib/components/Modal.svelte';
	import { guardUnsavedChanges, UNSAVED_NOTICE } from '#lib/unsavedChanges.js';
	import { UnsavedChangesNotice } from '@banto/forms';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '#lib/toast.svelte.js';
	import {
		dismissLegacyHubSelection,
		getHubCatalog,
		hubStatusLabel,
		type HubCatalog
	} from '#lib/banto/hubAdmin.js';
	import {
		ALLOWED_PERIOD_MS,
		registerTagsFromHub,
		type CollectionGroup,
		type PlcConnection
	} from '#lib/banto/tagRegistryAdmin.js';
	import {
		availabilityLabel,
		buildRegistration,
		defaultTarget,
		dialogDirty,
		filterHubTags,
		hubGroupsOf,
		hubTagAvailability,
		applyCatalogLoad,
		legacyNoticeText,
		registrationBlocker,
		registrationSuccessMessage,
		rowErrorsByExternalName,
		targetErrorMessages,
		type InitialSelection,
		type RegistrationTarget
	} from '#lib/banto/hubRegistration.js';

	interface Props {
		open: boolean;
		connections: PlcConnection[];
		groups: CollectionGroup[];
		onclose: () => void;
		/** 登録できた後（一覧の読み直し用）。 */
		onregistered: () => void;
	}

	let { open, connections, groups, onclose, onregistered }: Props = $props();

	const DIALOG_CONFIRM_CLOSE = '選択や入力を破棄してダイアログを閉じますか？';

	let catalog = $state<HubCatalog | null>(null);
	let loading = $state(false);
	let loadError = $state<string | null>(null);
	let query = $state('');
	let selected = $state<string[]>([]);
	let initial = $state<InitialSelection>({
		selected: [],
		showLegacyNotice: false,
		legacySkipped: []
	});
	let target = $state<RegistrationTarget>({ kind: 'new', name: '', periodMs: 1000 });
	let registering = $state(false);
	let dismissing = $state(false);
	let rowErrors = $state<Map<string, string[]>>(new Map());
	let targetErrors = $state<string[]>([]);
	/**
	 * 読み込みの世代。開き直し・「一覧を更新」のたびに進め、古い応答を捨てる
	 * （閉じた後や前回の読み込みの応答で、今のダイアログを書き換えない）。
	 */
	let loadGeneration = 0;

	const hubGroups = $derived(hubGroupsOf(connections, groups));
	const registeredSet = $derived(new Set(catalog?.registered ?? []));
	const selectedSet = $derived(new Set(selected));
	const visibleTags = $derived(filterHubTags(catalog?.tags ?? [], query));
	const selectableCount = $derived(
		(catalog?.tags ?? []).filter((tag) => hubTagAvailability(tag, registeredSet).selectable).length
	);
	const blocker = $derived(registrationBlocker(selected.length, target));
	const dirty = $derived(open && dialogDirty(selectedSet, initial.selected, target));
	const periodOptions = ALLOWED_PERIOD_MS.map((ms) => ({
		value: ms,
		label: ms >= 1000 ? `${ms / 1000}s` : `${ms}ms`
	}));

	guardUnsavedChanges({ isDirty: () => dirty, isSaving: () => registering });

	function errorMessage(err: unknown): string {
		if (isProviderError(err)) {
			return err.body.kind === 'validation'
				? err.body.field_errors.map((e) => e.message).join(' / ')
				: err.message;
		}
		return String(err);
	}

	/** この開いている間に、タグ一覧を 1 度でも読めたか（`applyCatalogLoad`）。 */
	let tagsRead = $state(false);

	async function load(): Promise<void> {
		loadGeneration += 1;
		const generation = loadGeneration;
		loading = true;
		loadError = null;
		try {
			const next = await getHubCatalog();
			if (generation !== loadGeneration || !open) return;
			catalog = next;
			// 読めなかった（`tags: null`）ときは選択を変えない。初めて読めたら事前チェック、
			// その後は今も選べるものだけ残す（判断は `applyCatalogLoad`）。
			const applied = applyCatalogLoad({ selected, initial, tagsRead }, next);
			selected = applied.selected;
			initial = applied.initial;
			tagsRead = applied.tagsRead;
		} catch (err) {
			if (generation !== loadGeneration || !open) return;
			// 読めなかったことを「タグ 0 件」に潰さない。
			loadError = errorMessage(err);
		} finally {
			if (generation === loadGeneration) loading = false;
		}
	}

	/** 開いた瞬間だけ初期化して読み込む（開いている間に親が再描画しても読み直さない）。 */
	let wasOpen = false;
	$effect(() => {
		const isOpen = open;
		if (isOpen === wasOpen) return;
		wasOpen = isOpen;
		if (!isOpen) {
			// 閉じたら飛んでいる読み込みを捨てる。
			loadGeneration += 1;
			return;
		}
		catalog = null;
		query = '';
		selected = [];
		initial = { selected: [], showLegacyNotice: false, legacySkipped: [] };
		tagsRead = false;
		target = defaultTarget(hubGroups);
		rowErrors = new Map();
		targetErrors = [];
		void load();
	});

	function toggle(externalName: string, checked: boolean): void {
		selected = checked
			? [...selected, externalName]
			: selected.filter((name) => name !== externalName);
		rowErrors = new Map();
	}

	/** 表示中（検索で絞った）の選べる行をまとめて選ぶ・外す。 */
	function toggleVisible(checked: boolean): void {
		const names = visibleTags
			.filter((tag) => hubTagAvailability(tag, registeredSet).selectable)
			.map((tag) => tag.externalName);
		const set = new Set(selected);
		for (const name of names) {
			if (checked) set.add(name);
			else set.delete(name);
		}
		selected = [...set];
		rowErrors = new Map();
	}

	function requestClose(): boolean {
		if (registering) return false;
		if (!dirty) return true;
		return window.confirm(DIALOG_CONFIRM_CLOSE);
	}

	async function dismissLegacy(): Promise<void> {
		if (dismissing) return;
		dismissing = true;
		try {
			await dismissLegacyHubSelection();
			const legacy = new Set(catalog?.legacySelection ?? []);
			// 事前チェックしたものだけ外す（利用者が自分で選んだものは残す）。
			selected = selected.filter((name) => !(legacy.has(name) && initial.selected.includes(name)));
			initial = { selected: [], showLegacyNotice: false, legacySkipped: [] };
			if (catalog) catalog = { ...catalog, legacySelection: [] };
			toastStore.push('success', '以前の選択を消しました。');
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			dismissing = false;
		}
	}

	async function register(): Promise<void> {
		if (registering || blocker !== null || !catalog?.tags) return;
		registering = true;
		rowErrors = new Map();
		targetErrors = [];
		try {
			const result = await registerTagsFromHub(
				buildRegistration(catalog.tags, selectedSet, registeredSet, target)
			);
			if (!result.ok) {
				rowErrors = rowErrorsByExternalName(result);
				targetErrors = targetErrorMessages(result);
				toastStore.push(
					'error',
					'登録できませんでした。エラーのある行を直してください（何も登録していません）。'
				);
				return;
			}
			toastStore.push('success', registrationSuccessMessage(result));
			onregistered();
			// 登録できたので未保存の入力は無い。確認を出さずに閉じる。
			selected = [];
			initial = { selected: [], showLegacyNotice: false, legacySkipped: [] };
			target = defaultTarget(hubGroups);
			onclose();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			registering = false;
		}
	}

	function onTargetKindChange(kind: 'existing' | 'new'): void {
		target =
			kind === 'existing'
				? { kind: 'existing', collectionGroupId: hubGroups[0]?.id ?? null }
				: { kind: 'new', name: '', periodMs: 1000 };
		targetErrors = [];
	}
</script>

<Modal
	{open}
	title="Hub から追加"
	width="760px"
	{dirty}
	onRequestClose={requestClose}
	onBlockedClose={() =>
		toastStore.push('info', '未保存の選択があります。閉じるには × を押してください。')}
	{onclose}
>
	<div class="hub-import">
		<p class="note">
			Hub のタグを選んで、このアプリのタグとして登録します。名前・データ型・単位・小数桁は Hub
			のものを複製し、アドレスは Hub
			のタグ名（外部名）になります。書き込みはできず、スケーリングも設定しません（Hub
			は工学値を送ります）。登録したタグは「収集を再起動」で記録が始まります。
		</p>
		<UnsavedChangesNotice pending={dirty} label={UNSAVED_NOTICE} />

		{#if initial.showLegacyNotice}
			<div class="legacy-notice" role="status">
				<p>{legacyNoticeText(initial)}</p>
				<button
					type="button"
					class="link"
					onclick={dismissLegacy}
					disabled={dismissing || registering}
				>
					以前の選択を使わない
				</button>
			</div>
		{/if}

		{#if loadError}
			<p class="load-error" role="alert">
				Hub のタグ一覧を読み込めませんでした（{loadError}）。タグが 0 件という意味ではありません。
				<button type="button" onclick={() => void load()} disabled={loading}> 再試行 </button>
			</p>
		{:else if catalog === null}
			<p class="loading">Hub のタグ一覧を読み込んでいます…</p>
		{:else if catalog.tags === null}
			<p class="load-error" role="alert">
				Hub のタグ一覧を読めませんでした（状態: {hubStatusLabel(catalog.status)}）。Hub
				への接続は「設定 &gt; Hub接続」で確認できます（管理者）。
				<button type="button" onclick={() => void load()} disabled={loading}>再試行</button>
			</p>
		{:else}
			<div class="toolbar">
				<label class="search">
					検索
					<input
						type="search"
						placeholder="タグ名・単位・型で絞り込み"
						bind:value={query}
						disabled={registering}
					/>
				</label>
				<button type="button" onclick={() => void load()} disabled={loading || registering}>
					一覧を更新
				</button>
			</div>
			<p class="note">
				Hub のタグ {catalog.tags.length} 件（選べるもの {selectableCount} 件）・選択中 {selected.length}
				件
			</p>
			{#if catalog.tags.length === 0}
				<p class="note">Hub にタグがありません。</p>
			{:else if visibleTags.length === 0}
				<p class="note">検索に一致するタグがありません。</p>
			{:else}
				<div class="bulk">
					<button
						type="button"
						class="link"
						onclick={() => toggleVisible(true)}
						disabled={registering}
					>
						表示中の選べるタグをすべて選ぶ
					</button>
					<button
						type="button"
						class="link"
						onclick={() => toggleVisible(false)}
						disabled={registering}
					>
						表示中の選択を外す
					</button>
				</div>
				<ul class="hub-tags" aria-label="Hub のタグ">
					{#each visibleTags as tag (tag.externalName)}
						{@const availability = hubTagAvailability(tag, registeredSet)}
						{@const errors = rowErrors.get(tag.externalName) ?? []}
						<li class:unavailable={!availability.selectable} class:has-error={errors.length > 0}>
							<label class="row">
								<input
									type="checkbox"
									checked={selectedSet.has(tag.externalName)}
									disabled={!availability.selectable || registering}
									onchange={(event) => toggle(tag.externalName, event.currentTarget.checked)}
								/>
								<span class="name">{tag.externalName}</span>
								<span class="meta">
									{tag.name} / {tag.dataType}{tag.unit ? ` / ${tag.unit}` : ''}
								</span>
								{#if !availability.selectable}
									<span class="reason">{availabilityLabel(availability, tag.dataType)}</span>
								{/if}
							</label>
							{#each errors as message, i (i)}
								<p class="row-error" role="alert">{message}</p>
							{/each}
						</li>
					{/each}
				</ul>
			{/if}

			<fieldset class="target" disabled={registering}>
				<legend>登録先の収集グループ（Hub 接続の下）</legend>
				{#if hubGroups.length > 0}
					<label class="choice">
						<input
							type="radio"
							name="hub-import-target"
							checked={target.kind === 'existing'}
							onchange={() => onTargetKindChange('existing')}
						/>
						既存のグループ
					</label>
					{#if target.kind === 'existing'}
						<select
							aria-label="登録先の収集グループ"
							value={target.collectionGroupId ?? ''}
							onchange={(event) => {
								const id = Number(event.currentTarget.value);
								target = { kind: 'existing', collectionGroupId: Number.isFinite(id) ? id : null };
							}}
						>
							{#each hubGroups as group (group.id)}
								<option value={group.id}>{group.name}</option>
							{/each}
						</select>
					{/if}
				{/if}
				<label class="choice">
					<input
						type="radio"
						name="hub-import-target"
						checked={target.kind === 'new'}
						onchange={() => onTargetKindChange('new')}
					/>
					新しいグループを作る{hubGroups.length === 0 ? '（Hub 接続も作成します）' : ''}
				</label>
				{#if target.kind === 'new'}
					<div class="new-group">
						<label>
							名前
							<input
								type="text"
								maxlength="100"
								value={target.name}
								oninput={(event) => {
									if (target.kind === 'new')
										target = { ...target, name: event.currentTarget.value };
								}}
							/>
						</label>
						<label>
							収集周期
							<select
								value={target.periodMs}
								onchange={(event) => {
									if (target.kind === 'new') {
										target = { ...target, periodMs: Number(event.currentTarget.value) };
									}
								}}
							>
								{#each periodOptions as option (option.value)}
									<option value={option.value}>{option.label}</option>
								{/each}
							</select>
						</label>
					</div>
				{/if}
				{#each targetErrors as message, i (i)}
					<p class="row-error" role="alert">{message}</p>
				{/each}
			</fieldset>

			<div class="actions">
				{#if blocker}
					<span class="note">{blocker}</span>
				{/if}
				<button type="button" onclick={register} disabled={blocker !== null || registering}>
					{registering ? '登録しています…' : `${selected.length} 件を登録`}
				</button>
			</div>
		{/if}
	</div>
</Modal>

<style>
	.hub-import {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.note {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.load-error,
	.row-error {
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	.legacy-notice {
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		padding: 0.5rem 0.75rem;
		font-size: 0.8rem;
	}

	.legacy-notice p {
		margin: 0 0 0.25rem;
	}

	.toolbar {
		display: flex;
		align-items: flex-end;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.search {
		display: flex;
		flex-direction: column;
		flex: 1 1 16rem;
		font-size: 0.8rem;
	}

	.bulk {
		display: flex;
		gap: 1rem;
	}

	button.link {
		padding: 0;
		background: none;
		border: none;
		color: var(--banto-primary);
		font: inherit;
		font-size: 0.8rem;
		text-decoration: underline;
		cursor: pointer;
	}

	button.link:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.hub-tags {
		margin: 0;
		padding: 0;
		list-style: none;
		max-height: 18rem;
		overflow-y: auto;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
	}

	.hub-tags li {
		padding: 0.3rem 0.5rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.hub-tags li:last-child {
		border-bottom: none;
	}

	.hub-tags li.unavailable .name,
	.hub-tags li.unavailable .meta {
		color: var(--banto-text-muted);
	}

	.hub-tags li.has-error {
		background: color-mix(in srgb, var(--banto-danger) 8%, transparent);
	}

	.row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.name {
		font-family: var(--banto-font-mono, monospace);
		font-size: 0.85rem;
	}

	.meta,
	.reason {
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.reason {
		margin-left: auto;
	}

	.target {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		padding: 0.5rem 0.75rem;
	}

	.target legend {
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.choice {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.85rem;
	}

	.new-group {
		display: flex;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	.new-group label {
		display: flex;
		flex-direction: column;
		font-size: 0.8rem;
	}

	.actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.75rem;
	}
</style>
