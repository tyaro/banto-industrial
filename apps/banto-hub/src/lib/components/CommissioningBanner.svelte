<script lang="ts">
	/**
	 * 試運転モード（未ロックダウン）中、全画面共通で常時表示する警告バナー
	 * （設計 §5.6 制約2・2026-08-30 オーナー決定）。
	 *
	 * 表示条件は `sessionStore.commissioningMode` の1つだけ - この値は
	 * `(app)/+layout.ts` のルートガードが `GET /api/commissioning/status`
	 * を確認した結果としてしか true にならない（`$lib/session.svelte.ts`
	 * の `enterCommissioningMode()` 参照）ため、ここでは追加の判定を持たず
	 * 素直に読むだけでよい。
	 *
	 * **閉じるボタン/dismiss 機能を意図的に持たない。** 設計上「試運転
	 * モードのまま出荷される」ことが最大のリスクとされている（docs/
	 * tag-server-design.md §5.6）ため、一度見た人がタブを閉じたり
	 * ページ遷移したりしても再び目に入ることが要件そのもの - `ToastHost.svelte`
	 * のような自動消滅/手動 dismiss 可能な通知と混同しないこと。ロック
	 * ダウンが完了すれば `commissioningMode` が false になり自然に消える
	 * （＝正しい「消し方」は設定画面でロックダウンを実行することだけ）。
	 */
	import { sessionStore } from '$lib/session.svelte';
</script>

{#if sessionStore.commissioningMode}
	<div class="commissioning-banner" role="alert">
		<span class="icon" aria-hidden="true">⚠️</span>
		<span>試運転モード: 認証なしで誰でも操作できます。運用開始前にロックダウンしてください。</span>
	</div>
{/if}

<style>
	.commissioning-banner {
		/* スクロールしても隠れないよう先頭に固定する - main 側が伸びて
		   ページ全体がスクロールしても、この警告だけは常に見える位置に
		   居続けなければならない（「常時表示」要件、閉じるボタンが無い
		   こととセットで初めて意味を持つ）。
		   z-index は敢えて低く抑える: `Drawer.svelte` の `.overlay`
		   （`position: fixed; inset: 0; z-index: 900`、タグ編集 Drawer 等が
		   画面全体を覆う）は viewport 最上部（y=0 付近）にも閉じるボタンを
		   置くため、この banner を高い z-index にすると sticky で画面最上部に
		   固定された banner が Drawer のクリックを物理的に奪ってしまう
		   （実際に e2e で回帰を確認した - Drawer の×ボタンがクリックできなく
		   なった）。Header/Sidebar（z-index 未指定 = 実質 0）より上、
		   Drawer(900)/CommandPalette・ToastHost・TreeContextMenu(1000) より
		   下、の間に収める。 */
		position: sticky;
		top: 0;
		z-index: 10;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		padding: 0.5rem 1rem;
		background: color-mix(in srgb, var(--banto-warning, #8a5a00) 22%, var(--banto-surface));
		color: var(--banto-warning, #8a5a00);
		border-bottom: 2px solid var(--banto-warning, #8a5a00);
		font-size: 0.85rem;
		font-weight: 700;
		text-align: center;
		flex-wrap: wrap;
	}

	.icon {
		font-size: 1rem;
		line-height: 1;
	}
</style>
