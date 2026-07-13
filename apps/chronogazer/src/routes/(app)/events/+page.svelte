<script lang="ts">
	/**
	 * イベント画面（R1-A: プレースホルダ）。
	 *
	 * 実データ（collect_events の一覧、banto の監査ログページの流儀）は
	 * R1-C（収集ランタイム統合）で入る。docs/r1-plan.md 参照。
	 */
</script>

<div class="page">
	<div class="page-header">
		<h2>イベント</h2>
	</div>

	<div class="empty-state">
		<p class="empty-message">この画面は R1-C フェーズで実装予定です</p>
		<p class="empty-hint">
			収集ランタイムの接続状態変化・断線復旧などのイベント一覧をここに表示します。
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
