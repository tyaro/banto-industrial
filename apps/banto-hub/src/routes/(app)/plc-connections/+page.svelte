<script lang="ts">
	/**
	 * PLC接続（plc_connections）CRUD 画面。relay-wright の
	 * `(app)/plc-connections/+page.svelte`（417行）を反復した実装指示どおりの
	 * シンプル型: BantoGrid一覧＋行クリックで Drawer を開く、viewer=閲覧のみ／
	 * editor以上=作成・編集・削除。relay-wright と異なり banto-hub には
	 * デモモードが無い（`tagRegistryAdmin.ts` 参照）ので、その分岐は持たない。
	 *
	 * T18-6a（TAG-UX-7/TAG-UX-8、2026-08-27 オーナー決定）: 作成・再設定の
	 * フォーム本体は `$lib/components/ConnectionDrawer.svelte` へ切り出した
	 * （tags ページのツリー右クリックからも同じ部品を開けるようにする狙い、
	 * 実装は T18-6d 別スライス）。このページは一覧の表示・行選択・Drawer の
	 * 開閉・作成直後の「次へ」導線バナーだけを持つ薄い実装に変わった。
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
	 * それをトーストで表示する（`ConnectionDrawer.svelte::handleDelete` が担う）。
	 *
	 * T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): `calc`/`mem` は
	 * `banto-hub` が起動時に自動プロビジョニングする予約接続
	 * （`protocol: "virtual"`）。バックエンドは編集・削除そのものを拒否する
	 * （`PlcConnectionService::update`/`delete`）ため、このページは一覧に
	 * 出しつつ行クリックでの Drawer を開かせず、その理由をトーストで示す
	 * （実装指示「一覧に出すが編集・削除不可の表示」）。新規作成フォームの
	 * プロトコル選択肢にも `"virtual"` を含めない — ユーザーが独自の
	 * virtual 接続を作る導線ではない（`ConnectionDrawer.svelte`/
	 * `plcConnectionForm.ts::PROTOCOL_OPTIONS` 参照）。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import ConnectionDrawer from '$lib/components/ConnectionDrawer.svelte';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listPlcConnections,
		isVirtualConnection,
		type PlcConnection
	} from '$lib/banto/tagRegistryAdmin';
	import { collectionGroupsHref } from '$lib/banto/tagOnboarding';

	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
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

	// --- Drawer 表示制御 -------------------------------------------------------
	// `drawerConnection` は「編集対象」。`null` かつ `drawerOpen` なら新規作成。
	let drawerOpen = $state(false);
	let drawerConnection: PlcConnection | null = $state(null);

	/**
	 * T18-2d（TAG-UX-A「PLC 作成後は次のグループ…へ進む CTA を表示する」）:
	 * 直近に作成した接続。作成成功直後だけ「次へ: 収集グループを作成」の
	 * CTA バナーを出すために保持する（一覧の他の行を作成/編集/削除しても
	 * 消えない - 意図的に「次へ進んでいない」間は出し続ける単純な設計）。
	 */
	let lastCreated: PlcConnection | null = $state(null);

	function openCreateDrawer(): void {
		drawerConnection = null;
		drawerOpen = true;
	}

	function selectConnection(c: PlcConnection): void {
		if (isVirtualConnection(c)) {
			toastStore.push(
				'error',
				`${c.name} は自動プロビジョニングされた予約接続のため編集・削除できません`
			);
			return;
		}
		drawerConnection = c;
		drawerOpen = true;
	}

	function closeDrawer(): void {
		drawerOpen = false;
	}

	async function handleDrawerSaved(saved: PlcConnection): Promise<void> {
		// 新規作成のときだけオンボーディングCTAを出す（再設定では出さない -
		// 旧実装の `lastCreated` はハンドラ内で「作成」のときにしか
		// 代入していなかったのと同じ区別を、ここでは `drawerConnection`
		// が保存前に null だったか（＝作成フローだったか）で判定する）。
		if (drawerConnection === null) {
			lastCreated = saved;
		}
		await reload();
	}

	async function handleDrawerDeleted(): Promise<void> {
		await reload();
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

<div class="page">
	<h2>PLC接続</h2>

	{#if canWrite}
		<div class="toolbar">
			<button type="button" onclick={openCreateDrawer}>新規作成</button>
		</div>
	{/if}

	{#if lastCreated}
		<!--
			T18-2d（TAG-UX-A「PLC 作成後は次のグループ…へ進む CTA を表示する」）:
			接続作成の直後に、下の収集グループ作成への導線を出す（サイドバー
			探索なしで次工程へ進めるようにする）。文言は「登録が完了しました」
			とし、上の成功トースト（`作成しました`）と部分一致しないように
			する（tags/+page.svelte 側の同種バナーで `getByText('作成しました')`
			が strict mode violation を起こした実測回帰、2026-08-12、PR #135
			CI、と同じ理由の予防）。
		-->
		<div class="onboarding-banner">
			<span>「{lastCreated.name}」の登録が完了しました。</span>
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
				? '行をクリックすると接続の再設定用 Drawer が開きます。'
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
</div>

<ConnectionDrawer
	open={drawerOpen}
	connection={drawerConnection}
	existingNames={connections.map((c) => c.name)}
	onClose={closeDrawer}
	onSaved={handleDrawerSaved}
	onDeleted={handleDrawerDeleted}
/>

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

	.toolbar {
		display: flex;
		justify-content: flex-end;
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

	button.secondary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}
</style>
