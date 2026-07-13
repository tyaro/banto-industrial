<script lang="ts">
	/**
	 * ヒストリカル画面（R1-A: プレースホルダ）。
	 *
	 * 実装は R2 フェーズ（過去データの期間指定表示、banto-tsquery の
	 * read_decimated を使ったサーバー側間引き）。docs/r1-plan.md 参照。
	 */
</script>

<div class="page">
	<div class="page-header">
		<h2>ヒストリカル</h2>
	</div>

	<div class="empty-state">
		<p class="empty-message">この画面は R2 フェーズで実装予定です</p>
		<p class="empty-hint">
			過去データの期間指定表示・エクスポートは、収集ランタイム（R1-C）と監視画面（R1-D）の後に追加します。
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
