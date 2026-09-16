/**
 * #381 レビュー対応8回目（2026-09-16）: モーダル層（`Drawer.svelte` /
 * `Modal.svelte`）の**フォーカストラップ**。`escLayering.ts`（Esc の層の約束）・
 * `focusRestore.ts`（閉じたときのフォーカス戻し）と同じ並びの小モジュールで、
 * banto-hub のストア・型を import しない（`lib/components/` 直下の規約）。
 *
 * ## なぜ入れたか
 *
 * `Drawer.svelte` は当初「開いたら先頭要素へフォーカス」だけで、Tab の循環は
 * 「需要を見て追加する」としていた。その需要が出た: フォーカスがパネルの外へ
 * 出られると、**オーバーレイの裏にある起動ボタンへ Tab で到達して別の層を
 * 開けてしまう**（接続 Drawer を開いたまま「収集グループを追加」に届く等）。
 * そこから「同じ z 順の層が2つ開いて Esc がどちらも効かない」「未保存の入力を
 * 捨てずにどう排他するか」という症状が連鎖して出ていた（#381 のレビュー
 * 5〜8回目）。入口をひとつ塞ぐのが根本対策になる。
 *
 * ## 何をするか
 *
 * パネル内のフォーカス可能要素を列挙し、**末尾で Tab → 先頭へ / 先頭で
 * Shift+Tab → 末尾へ**折り返す。列挙のセレクタは両部品の `focusFirst`（開いた
 * 直後に先頭要素へフォーカスする既存処理）と同じものを使い、`disabled` と
 * 不可視（矩形なし）と `inert` の中だけ除く - フォーム部品の出入りで
 * 顔ぶれが変わるので、**毎回数え直す**（開いた時点でキャッシュしない）。
 */

import { hasVisibleLayerAbove } from './escLayering';

/** 両部品の `focusFirst` と同じ列挙。 */
export const FOCUSABLE_SELECTOR =
	'input, select, textarea, button, a[href], [tabindex]:not([tabindex="-1"])';

/** `panel` の中でいまフォーカスを受けられる要素（DOM 順）。 */
export function focusablesIn(panel: HTMLElement): HTMLElement[] {
	return [...panel.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)].filter(
		(el) => !el.hasAttribute('disabled') && !el.closest('[inert]') && el.getClientRects().length > 0
	);
}

/**
 * `panel` の `keydown` に張る Tab の折り返し。Tab 以外は何もしない。
 *
 * フォーカス可能要素が1つも無いときは、パネルの外へ出さないことだけを担保する
 * （`preventDefault` するがフォーカスは動かさない）。
 */
function handleTrapKeydown(panel: HTMLElement, event: KeyboardEvent): void {
	if (event.key !== 'Tab') return;
	const items = focusablesIn(panel);
	if (items.length === 0) {
		event.preventDefault();
		return;
	}
	const first = items[0];
	const last = items[items.length - 1];
	const active = document.activeElement;
	if (event.shiftKey) {
		if (active === first) {
			event.preventDefault();
			last.focus();
		}
	} else if (active === last) {
		event.preventDefault();
		first.focus();
	}
}

/**
 * `panel` にフォーカストラップを張り、外す関数を返す（#381 レビュー対応13回目で
 * **入口をこれ1つに統一**した - Drawer / Modal / CommandPalette で同じ2つの
 * リスナーを書き写さない）。
 *
 * 2段構え:
 * 1. **パネルの `keydown`**: Tab / Shift+Tab をパネル内で循環させる
 *    （末尾 → 先頭 / 先頭 → 末尾）。
 * 2. **document の `focusin`**: パネルの外へ着地したフォーカスを先頭要素へ
 *    引き戻す。1 だけでは足りない - **プログラム的な `focus()` などで一度外へ
 *    出てしまうと、その後の Tab はパネルの外で発生するのでパネルの keydown
 *    リスナーには届かない**（外に出た状態からの復帰経路が無い）。
 *
 * ただし**自分より手前の層が出ているあいだは引き戻さない**
 * （{@link hasVisibleLayerAbove}）: 入れ子（Drawer の上にコマンドパレット等）で
 * 下の層が上の層からフォーカスを奪い合うのを避けるため。
 */
export function attachFocusTrap(panel: HTMLElement): () => void {
	const onKeydown = (event: KeyboardEvent): void => handleTrapKeydown(panel, event);
	const onFocusIn = (event: FocusEvent): void => {
		const target = event.target;
		if (target instanceof Node && panel.contains(target)) return;
		if (hasVisibleLayerAbove({ except: panel })) return;
		const items = focusablesIn(panel);
		(items[0] ?? panel).focus();
	};
	panel.addEventListener('keydown', onKeydown);
	document.addEventListener('focusin', onFocusIn);
	return () => {
		panel.removeEventListener('keydown', onKeydown);
		document.removeEventListener('focusin', onFocusIn);
	};
}
