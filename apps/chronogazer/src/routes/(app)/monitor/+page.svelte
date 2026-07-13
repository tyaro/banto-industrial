<script lang="ts">
	/**
	 * 監視画面（R1-A: プレースホルダ）。ログイン後の既定ページ（`routes/+page.ts`、
	 * `routes/login/+page.svelte`）。
	 *
	 * R1-B で表示グループ（ペン割当・表示種別）CRUD が入り、R1-D で実データの
	 * 4種表示（トレンド/デジタル/バー/計器）に置き換わる（docs/r1-plan.md）。
	 * それまでは表示グループが1件も無い状態が常なので、空状態メッセージのみ
	 * 表示する。
	 */
</script>

<div class="page">
	<div class="page-header">
		<h2>監視</h2>
	</div>

	<div class="empty-state">
		<p class="empty-message">表示グループが未設定です</p>
		<p class="empty-hint">
			設定画面から表示グループを作成すると、ここにトレンド・デジタル・バー・計器表示が並びます。
		</p>
	</div>
</div>

<style>
	.page {
		height: calc(100vh - var(--banto-shell-header-height) - 2.5rem);
		display: flex;
		flex-direction: column;
		min-height: 0;
		gap: 0.5rem;
	}

	.page-header {
		flex: 0 0 auto;
	}

	.page-header h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.empty-state {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
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
</style>
