<script lang="ts">
	/**
	 * PLC接続（plc_connections）CRUD 画面。relay-wright の
	 * `(app)/plc-connections/+page.svelte`（417行）を反復した実装指示どおりの
	 * シンプル型: BantoGrid一覧＋行クリックで下に編集パネル、viewer=閲覧のみ／
	 * editor以上=作成・編集・削除。relay-wright と異なり banto-hub には
	 * デモモードが無い（`tagRegistryAdmin.ts` 参照）ので、その分岐は持たない。
	 *
	 * プロトコルは "slmp" と "modbus-tcp" の select（banto-tags のレジストリ
	 * としては両方正当）。banto-collect の `build_config`
	 * （crates/banto-collect/src/config.rs の `parse_protocol`）は
	 * I8（2026-08-05実装）で両方とも実際に収集を組み立てられるようになった -
	 * slmp を選んで登録すればそのまま収集される。既定値は modbus-tcp のまま
	 * （デバッグしやすさを優先した既存の選定 - docs/plan.md I2 の判断を参照）。
	 *
	 * 削除は、収集グループが参照している場合にサービス層の Validation
	 * エラー（「…収集グループが N 件あるため削除できません」）で拒否されるので、
	 * それをトーストで表示する。
	 *
	 * T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): `calc`/`mem` は
	 * `banto-hub` が起動時に自動プロビジョニングする予約接続
	 * （`protocol: "virtual"`）。バックエンドは編集・削除そのものを拒否する
	 * （`PlcConnectionService::update`/`delete`）ため、このページは一覧に
	 * 出しつつ行クリックでの編集パネルを開かせず、その理由をトーストで示す
	 * （実装指示「一覧に出すが編集・削除不可の表示」）。新規作成フォームの
	 * プロトコル選択肢にも `"virtual"` を含めない — ユーザーが独自の
	 * virtual 接続を作る導線ではない。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listPlcConnections,
		createPlcConnection,
		updatePlcConnection,
		deletePlcConnection,
		isVirtualConnection,
		testPlcConnection,
		WORD_ORDER_OPTIONS,
		type PlcConnection,
		type PlcConnectionInput,
		type PlcConnectionTestResult,
		type PlcProtocol,
		type SlmpWordOrder
	} from '$lib/banto/tagRegistryAdmin';
	import { collectionGroupsHref } from '$lib/banto/tagOnboarding';

	// "virtual" is intentionally NOT offered here (this module's doc comment)
	// - the two virtual connections are auto-provisioned by the backend, not
	// created through this form.
	const protocolOptions: { value: PlcProtocol; label: string }[] = [
		{ value: 'modbus-tcp', label: 'Modbus TCP' },
		{ value: 'slmp', label: 'SLMP（MELSEC）' }
	];

	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	/** 編集フォーム状態（作成/編集共通）。数値入力は文字列で保持し、空欄=未設定。 */
	interface FormState {
		name: string;
		protocol: PlcProtocol;
		host: string;
		port: string;
		unitId: string;
		enabled: boolean;
		simulation: boolean;
		wordOrder: SlmpWordOrder;
	}

	function blankForm(): FormState {
		return {
			name: '',
			// バックエンドの既定（PlcConnectionPayload の default_plc_protocol）
			// と一致させる。
			protocol: 'modbus-tcp',
			host: '',
			port: '502',
			unitId: '1',
			enabled: true,
			simulation: false,
			// P3-b（監査指摘 2026-08-12）: バックエンドの既定
			// （default_plc_word_order / SlmpConfig::default().word_order）と
			// 一致させる。
			wordOrder: 'low_high'
		};
	}

	function formFromConnection(c: PlcConnection): FormState {
		return {
			name: c.name,
			protocol: c.protocol,
			host: c.host,
			port: String(c.port),
			unitId: String(c.unitId),
			enabled: c.enabled,
			simulation: c.simulation,
			wordOrder: c.wordOrder
		};
	}

	function toInput(form: FormState): PlcConnectionInput {
		return {
			name: form.name,
			protocol: form.protocol,
			host: form.host,
			port: Number(form.port),
			unitId: Number(form.unitId),
			enabled: form.enabled,
			simulation: form.simulation,
			wordOrder: form.wordOrder
		};
	}

	/**
	 * T12 (docs/ux-plan.md §4): 接続テストの実行状態。作成・編集フォームの
	 * それぞれが独立した `TestState` を持つ（片方のテスト中でももう片方を
	 * 操作でき、結果表示も混ざらない）。
	 */
	interface TestState {
		testing: boolean;
		result: PlcConnectionTestResult | null;
	}

	function blankTestState(): TestState {
		return { testing: false, result: null };
	}

	/**
	 * `connectionId` は「保存済み接続の編集フォームからのテスト」のときだけ
	 * `selected.id` を渡す（作成フォームは常に `undefined`）。`testState` は
	 * 呼び出し元（作成/編集の各セクション）が持つ状態を直接渡してもらい、
	 * ここで書き換える — `connectionFields` スニペットは作成・編集で共有
	 * されているため、状態も呼び出し元から注入する形にした。
	 */
	async function runConnectionTest(
		form: FormState,
		connectionId: number | undefined,
		testState: TestState
	): Promise<void> {
		if (testState.testing) return; // 多重クリック防止
		testState.testing = true;
		testState.result = null;
		try {
			testState.result = await testPlcConnection({
				protocol: form.protocol,
				host: form.host,
				port: Number(form.port),
				unitId: Number(form.unitId),
				simulation: form.simulation,
				connectionId
			});
		} catch (err) {
			// 401/403・CSRF拒否・ネットワークエラーなど（`ok: false` はここに来ない
			// 通常応答 — 上の try 内で result にそのまま入る）。
			toastStore.push('error', errorMessage(err));
		} finally {
			testState.testing = false;
		}
	}

	let connections: PlcConnection[] = $state([]);
	let loading = $state(false);

	async function reload(): Promise<void> {
		loading = true;
		try {
			connections = await listPlcConnections();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	// --- create ---
	let createForm = $state(blankForm());
	let createErrors: Record<string, string> = $state({});
	let creating = $state(false);
	let createTestState: TestState = $state(blankTestState());
	/**
	 * T18-2d（TAG-UX-A「PLC 作成後は次のグループ…へ進む CTA を表示する」）:
	 * 直近に作成した接続。作成成功直後だけ「次へ: 収集グループを作成」の
	 * CTA バナーを出すために保持する（一覧の他の行を作成/編集/削除しても
	 * 消えない - 意図的に「次へ進んでいない」間は出し続ける単純な設計）。
	 */
	let lastCreated: PlcConnection | null = $state(null);

	function applyFieldErrors(err: unknown): Record<string, string> | null {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const map: Record<string, string> = {};
			for (const fe of err.body.field_errors) map[fe.field] = fe.message;
			return map;
		}
		return null;
	}

	async function handleCreate(): Promise<void> {
		creating = true;
		createErrors = {};
		try {
			const created = await createPlcConnection(toInput(createForm));
			toastStore.push('success', '作成しました');
			createForm = blankForm();
			createTestState = blankTestState();
			lastCreated = created;
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) createErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			creating = false;
		}
	}

	// --- edit ---
	let selected: PlcConnection | null = $state(null);
	let editForm = $state(blankForm());
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);
	let editTestState: TestState = $state(blankTestState());

	function selectConnection(c: PlcConnection): void {
		if (isVirtualConnection(c)) {
			toastStore.push(
				'error',
				`${c.name} は自動プロビジョニングされた予約接続のため編集・削除できません`
			);
			return;
		}
		selected = c;
		editForm = formFromConnection(c);
		editErrors = {};
		// 別の接続に切り替えたら、直前の接続テスト結果は無関係になるので消す
		// （表示が残ると「今開いている接続のテスト結果」に見えて誤解を招くため）。
		editTestState = blankTestState();
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updatePlcConnection(selected.id, toInput(editForm));
			toastStore.push('success', '更新しました');
			selected = updated;
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) editErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	async function handleDelete(): Promise<void> {
		if (!selected) return;
		if (!window.confirm(`${selected.name} を削除しますか？`)) return;
		try {
			await deletePlcConnection(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			await reload();
		} catch (err) {
			// 収集グループが参照している場合はサービス層の分かりやすい
			// Validation エラー（件数入り）がここに来る。
			toastStore.push('error', errorMessage(err));
		}
	}

	const columns: GridColumn<PlcConnection>[] = [
		{ id: 'id', header: 'ID', accessor: 'id', width: 60, align: 'right' },
		{
			id: 'name',
			header: '名前',
			accessor: 'name',
			width: 160,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'protocol',
			header: 'プロトコル',
			accessor: 'protocol',
			width: 110,
			format: (v) => (v === 'virtual' ? 'virtual（予約）' : String(v))
		},
		{ id: 'host', header: 'ホスト', accessor: 'host', width: 140 },
		{ id: 'port', header: 'ポート', accessor: 'port', width: 80, align: 'right' },
		{ id: 'unitId', header: 'ユニットID', accessor: 'unitId', width: 90, align: 'right' },
		{
			// P3-b（監査指摘 2026-08-12）: SLMP 以外では無意味なので "—" にする
			// （`unitId` 列は数値をそのまま出しているが、こちらは選択肢が2値しか
			// ないぶん「該当なし」を明示したほうが読み違いにくい）。
			id: 'wordOrder',
			header: 'ワード順',
			accessor: 'wordOrder',
			width: 100,
			format: (v, row) => (row.protocol === 'slmp' ? String(v) : '—')
		},
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		},
		{
			// T9-2 (docs/ux-plan.md §1): シミュレーションモードは事故防止の
			// 最優先項目 - 一覧で一目で気づけることが要件。GridColumn.format は
			// プレーンテキストしか返せない（@banto/grid-svelte の types.ts
			// 参照、cellClass 相当のフックは無い）ので、テキスト自体に警告記号を
			// 入れつつ、行全体の強調は BantoGrid の rowClass（下記
			// connectionRowClass）+ 呼び出し側 :global() スタイルで行う -
			// audit-log ページの `audit-row-alert` と同じパターン。
			// virtual（calc/mem）は simulation が常に false だが「該当なし」を
			// 「明示的にオフ」と区別するため空欄にする。
			id: 'simulation',
			header: 'シミュレーション',
			accessor: 'simulation',
			width: 130,
			format: (v, row) => {
				if (isVirtualConnection(row)) return '';
				return v ? '⚠ シミュレーション中' : '—';
			}
		},
		{
			id: 'editable',
			header: '編集',
			accessor: (row) => (isVirtualConnection(row) ? '不可（自動）' : '可'),
			width: 100
		}
	];

	/**
	 * T9-2: simulation=true の実接続行を BantoGrid の rowClass 経由で強調する
	 * （spec M14 の audit-log と同じ仕組み。上の `simulation` 列コメント参照）。
	 */
	function connectionRowClass(c: PlcConnection): string | undefined {
		return c.simulation && !isVirtualConnection(c) ? 'sim-row' : undefined;
	}
</script>

{#snippet connectionFields(
	form: FormState,
	errors: Record<string, string>,
	connectionId: number | undefined,
	testState: TestState
)}
	<div class="form-grid">
		<label class="field">
			名前
			<input type="text" bind:value={form.name} />
			{#if errors.name}<span class="err">{errors.name}</span>{/if}
		</label>
		<label class="field">
			プロトコル
			<select bind:value={form.protocol}>
				{#each protocolOptions as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.protocol}<span class="err">{errors.protocol}</span>{/if}
		</label>
		<label class="field">
			ホスト
			<input type="text" bind:value={form.host} placeholder="192.168.1.10" />
			{#if errors.host}<span class="err">{errors.host}</span>{/if}
		</label>
		<label class="field">
			ポート
			<input type="number" min="1" max="65535" bind:value={form.port} />
			{#if errors.port}<span class="err">{errors.port}</span>{/if}
		</label>
		<label class="field">
			ユニットID
			<input type="number" min="0" max="255" bind:value={form.unitId} />
			<span class="hint">Modbus 用のスレーブID（0〜255）。SLMP では未使用（既定 1 のまま）。</span>
			{#if errors.unitId}<span class="err">{errors.unitId}</span>{/if}
		</label>
		{#if form.protocol === 'slmp'}
			<label class="field">
				ワード順
				<select bind:value={form.wordOrder}>
					{#each WORD_ORDER_OPTIONS as opt (opt.value)}
						<option value={opt.value}>{opt.label}</option>
					{/each}
				</select>
				<span class="hint">
					32bit値（u32/f32等）の上位/下位ワードの並び。機種のマニュアルで確認してください —
					間違えると値が化けます（上位/下位が入れ替わります）。
				</span>
				{#if errors.wordOrder}<span class="err">{errors.wordOrder}</span>{/if}
			</label>
		{/if}
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.simulation} />
			シミュレーションモード
		</label>
		<span class="hint sim-hint">
			実PLCの代わりに内蔵シミュレータに接続します（開発・検証用）。本番運用では有効にしないでください。
		</span>
	</div>
	<div class="test-connection">
		<button
			type="button"
			class="test-btn"
			onclick={() => runConnectionTest(form, connectionId, testState)}
			disabled={testState.testing}
		>
			{#if testState.testing}<span class="spinner" aria-hidden="true"></span>{/if}
			接続テスト
		</button>
		{#if testState.testing}
			<span class="test-result testing">テスト中…</span>
		{:else if testState.result}
			{#if testState.result.ok}
				<span class="test-result ok">接続成功（応答 {testState.result.elapsedMs}ms）</span>
			{:else}
				<span class="test-result error"
					>{testState.result.error?.message ?? '接続に失敗しました'}</span
				>
			{/if}
		{/if}
	</div>
{/snippet}

<div class="page">
	<h2>PLC接続</h2>

	{#if canWrite}
		<section class="create">
			<h3>新規作成</h3>
			{@render connectionFields(createForm, createErrors, undefined, createTestState)}
			<button type="button" onclick={handleCreate} disabled={creating}>作成</button>
		</section>
	{/if}

	{#if lastCreated}
		<!--
			T18-2d（TAG-UX-A「PLC 作成後は次のグループ…へ進む CTA を表示する」）:
			接続作成の直後に、上の接続テスト・下の収集グループ作成への導線を
			まとめて出す（サイドバー探索なしで次工程へ進めるようにする）。
		-->
		<div class="onboarding-banner">
			<span>「{lastCreated.name}」を作成しました。</span>
			<a class="onboarding-cta" href={collectionGroupsHref(lastCreated.id)}
				>次へ: 収集グループを作成</a
			>
			<button type="button" class="secondary" onclick={() => (lastCreated = null)}>閉じる</button>
		</div>
	{/if}

	<section class="list">
		<h3>一覧</h3>
		<p class="note">
			{canWrite
				? '行をクリックすると下に編集パネルが表示されます。'
				: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
		</p>
		<p class="note">
			calc/mem は演算タグ・内部タグ用にサーバーが自動作成する予約接続です（編集・削除不可）。
		</p>
		{#if loading && connections.length === 0}
			<p class="loading">読み込み中…</p>
		{:else}
			<div class="grid-wrap">
				<BantoGrid
					rows={connections}
					{columns}
					getRowId={(c) => c.id}
					rowClass={connectionRowClass}
					onRowClick={canWrite ? selectConnection : undefined}
				/>
			</div>
		{/if}
	</section>

	{#if selected && canWrite}
		<section class="detail">
			<h3>{selected.name} を編集</h3>
			{@render connectionFields(editForm, editErrors, selected.id, editTestState)}
			<div class="actions">
				<button type="button" onclick={saveEdit} disabled={saving}>保存</button>
				<button type="button" class="danger" onclick={handleDelete}>削除</button>
			</div>
		</section>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 860px;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	h3 {
		margin: 0 0 0.75rem;
		font-size: 0.95rem;
	}

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.grid-wrap {
		height: 320px;
	}

	.form-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
		gap: 0.75rem;
		margin-bottom: 0.75rem;
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

	.hint {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	/* T9-2: シミュレーションモードのチェックボックス直下の注意書き。フォーム
	   グリッドの1マスに収まる長さではないので全幅を使う。 */
	.sim-hint {
		grid-column: 1 / -1;
		margin-top: -0.4rem;
		color: var(--banto-warning);
	}

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
	}

	/*
	 * T9-2 (docs/ux-plan.md §1): シミュレーション中の接続行を一覧で一目で
	 * 気づけるよう強調する。@banto/grid-svelte の BantoGrid.svelte 内部
	 * コメント（spec M14）が明示する使い方どおり、rowClass で付与した
	 * `.sim-row` を呼び出し側の :global() セレクタで直接スタイリングする
	 * （grid 内部の DOM は Svelte のスコープドCSSがこのコンポーネントの外
	 * なので :global が必要）。監査ログページの `.audit-row-alert`（境界線の
	 * みで --banto-danger）より一段強く、背景色も付けて誤操作防止の
	 * 最優先項目として目立たせる。--banto-danger ではなく --banto-warning
	 * を使うのは、この状態が「エラー」ではなく「注意すべき設定」であるため
	 * （status ページの config-error/quality-bad は danger を使っており、
	 * 意味を分けるためにここは warning にした）。
	 */
	:global(.row.sim-row) {
		background: color-mix(in srgb, var(--banto-warning) 12%, transparent);
		border-left: 3px solid var(--banto-warning);
	}

	:global(.row.sim-row .cell[data-cell-field='simulation']) {
		color: var(--banto-warning);
		font-weight: 600;
	}

	.actions {
		display: flex;
		gap: 0.75rem;
	}

	/* T18-2d（TAG-UX-A）: 作成直後の「次へ」導線バナー。 */
	.onboarding-banner {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.75rem;
		padding: 0.6rem 0.9rem;
		border: 1px solid var(--banto-primary);
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		font-size: 0.85rem;
	}

	.onboarding-cta {
		padding: 0.3rem 0.75rem;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		font-size: 0.8rem;
		text-decoration: none;
		white-space: nowrap;
	}

	.onboarding-cta:hover {
		background: var(--banto-primary-hover);
	}

	button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text-muted);
	}

	button.secondary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}

	/* T12 (docs/ux-plan.md §4): 接続テストボタン + インライン結果表示。 */
	.test-connection {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin-bottom: 0.75rem;
	}

	.test-btn {
		display: inline-flex;
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

	.spinner {
		width: 0.85rem;
		height: 0.85rem;
		border: 2px solid color-mix(in srgb, var(--banto-text) 25%, transparent);
		border-top-color: var(--banto-text);
		border-radius: 50%;
		animation: spin 0.7s linear infinite;
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

	.test-result {
		font-size: 0.8rem;
	}

	.test-result.testing {
		color: var(--banto-text-muted);
	}

	.test-result.ok {
		color: var(--banto-success, #1a7f37);
		font-weight: 600;
	}

	.test-result.error {
		color: var(--banto-danger);
		font-weight: 600;
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

	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}

	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}
</style>
