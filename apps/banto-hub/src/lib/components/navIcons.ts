/**
 * ナビ項目の絵文字（`NavItem.icon`）を `@banto/ui` の `UiIconComponent` にする
 * （#1159、ドックの `LauncherBar` は部品のアイコンを受ける）。admin-template の
 * `navIcons.ts` は lucide のアイコンを引くが、Hub のアイコンは絵文字のプレース
 * ホルダなので `NavEmojiIcon.svelte` に絵文字を束ねる。絵文字ごとに 1 度だけ作る
 * （毎回作り直すと、導出のたびにアイコンが付け替わって描き直される）。
 */
import type { UiIconComponent } from '@banto/ui';
import NavEmojiIcon from './NavEmojiIcon.svelte';

const cache = new Map<string, UiIconComponent>();

export function navIcon(emoji: string): UiIconComponent {
	let icon = cache.get(emoji);
	if (!icon) {
		const make: UiIconComponent = ((anchor, props) =>
			NavEmojiIcon(anchor, { ...props, emoji })) as UiIconComponent;
		cache.set(emoji, make);
		icon = make;
	}
	return icon;
}
