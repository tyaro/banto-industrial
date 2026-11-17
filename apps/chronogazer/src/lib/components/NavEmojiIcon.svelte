<script lang="ts">
	/**
	 * ナビ項目の絵文字アイコンを `@banto/ui` の `UiIconComponent`（ドックの
	 * `LauncherBar` が受ける形）として描く。ChronoGazer のアイコンは絵文字のプレースホルダ
	 * （`navigation.ts` の `NavItem.icon`）なので、絵文字を渡す薄い部品。
	 * `navIcons.ts` の `navIcon()` が絵文字ごとに束ねて `UiIconComponent` にする。
	 * 装飾なので既定で `aria-hidden`（項目の名前はラベルが担う）。
	 */
	interface Props {
		emoji: string;
		size?: number;
		'aria-hidden'?: 'true' | 'false' | boolean;
	}

	let { emoji, size = 20, 'aria-hidden': ariaHidden = true }: Props = $props();
</script>

<span class="emoji" aria-hidden={ariaHidden} style:font-size="{size}px">{emoji}</span>

<style>
	.emoji {
		display: inline-block;
		line-height: 1;
		text-align: center;
	}
</style>
