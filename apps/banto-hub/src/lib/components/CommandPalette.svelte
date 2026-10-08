<script lang="ts">
	/**
	 * コマンドパレット（Ctrl+K）のアプリ側の半分（banto #220 段階 3、2026-10-08）。
	 * ダイアログ本体（検索欄・グループ分けした一覧・キー操作・フォーカス
	 * トラップと戻し・window で受ける Esc・外側クリック・見た目）は `@banto/ui`
	 * の `CommandPalette`（banto ADR-0018 §8・段階 2b）。ここに残すのは
	 * banto-hub を知っている部分だけ: コマンドの一覧（`#lib/commands.ts`）、
	 * admin-core の点数付き検索、最近使ったコマンドの記録、失敗の通知、
	 * 閉じたときの戻し先が死んでいたときの代わり（ヘッダーの先頭ボタン）。
	 *
	 * `(app)/+layout.svelte` が `commandPaletteStore.open` のあいだだけ
	 * マウントする（`{#if}`）ので、開くたびに新しいインスタンスになり、
	 * コマンドの一覧と最近使った記録は開いた時点のものを読む。
	 *
	 * ## 層の約束（`escLayering.ts` の doc が正）との合わせ方
	 *
	 * `@banto/ui` のパレットは #381 の約束 1〜5 を banto-hub から移したもので、
	 * 層の印（`role="dialog"` / `role="menu"` / `data-esc-layer` /
	 * `data-layer-inactive`）も同じものを見る。z-index は `--banto-z-overlay`
	 * （1000、以前の直書きと同じ値）。
	 *
	 * - Esc（約束 1〜3）: window で受け、閉じるときは `preventDefault` する。
	 *   **自分より手前の層（z が大きい、同じ z なら DOM で後ろ）にだけ譲る**。
	 *   以前のこの部品は「譲る相手を見ない」実装だった（パレットが最上位で、
	 *   z だけ見る判定が無かったため）。banto 側は z 順で比べるので、下にある
	 *   Drawer/Modal（900）・オフキャンバスサイドバー（710）を手前と誤認しない。
	 *   同じ z（1000）の `TreeContextMenu` とは同時に出さない
	 *   （`(app)/+layout.svelte` の `Ctrl+K` の抑止）のは従来どおり。
	 * - フォーカストラップ（約束 4）: Tab の循環 + document の `focusin` の
	 *   引き戻しの 2 段構え（`focusTrap.ts` と同じ形）。
	 * - フォーカスの戻し（約束 5）: 閉じたら開いた元へ、`tick()` の後に判定する
	 *   （ナビ系コマンドの `goto()` → `afterNavigate` でサイドバーが畳まれ、
	 *   戻し先が同じ流れの中で `inert` になるため）。戻し先が消えている /
	 *   `inert` / 不可視なら `focusFallback`（ヘッダーの先頭ボタン ☰ - 常設で
	 *   どの画面にもある）へ。`<body>` には落とさない。
	 */
	import { CommandPalette } from '@banto/ui';
	import { isProviderError, notify, searchCommands, type PaletteCommand } from '@banto/admin-core';
	import { buildCommands, loadRecentCommandIds, recordRecentCommand } from '#lib/commands.js';
	import { commandPaletteStore } from '#lib/commandPalette.svelte.js';

	// 開くたびにマウントされるので、どちらも開いた時点の値。
	const commands = buildCommands();
	const recentIds = loadRecentCommandIds();

	// admin-core の点数付き検索（最近使ったものは同点の並びと空の検索語で先頭に
	// 来る）。最近使ったものは「並び順」であって別の見出しではないので、
	// パッケージの `recentIds`（「最近使ったもの」の見出し）は使わない - 以前の
	// 表示と同じ。
	function search(query: string, items: readonly PaletteCommand[]): PaletteCommand[] {
		return searchCommands([...items], query, recentIds);
	}

	// 実行中は行が無効になり、終わるとパッケージが閉じる（下の `onClose`）。
	// 失敗はここで通知し、投げ直さない。
	async function execute(command: PaletteCommand): Promise<void> {
		try {
			await command.run();
		} catch (err) {
			notify('error', isProviderError(err) ? err.message : String(err));
		}
		recordRecentCommand(command.id);
	}
</script>

<CommandPalette
	open
	items={commands}
	{search}
	onExecute={execute}
	onClose={() => commandPaletteStore.hide()}
	focusFallback={() => document.querySelector<HTMLElement>('header button')}
/>
