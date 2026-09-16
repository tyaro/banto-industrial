/**
 * #378 / #381 レビュー対応（2026-09-16）: **Esc の「層の約束」**をここ1箇所に
 * 集約する。`SplitPane.svelte`（狭幅の退避ツリー）と `(app)/+layout.svelte`
 * （オフキャンバスサイドバー）の両方がこれを使う - 同じ説明・同じ判定を2箇所に
 * 置かない。
 *
 * ## 約束
 *
 * 重なって出ている UI は **Esc で上から1層ずつ畳む**。そのために、
 *
 * 1. **閉じる側はイベントを消費する**（`event.preventDefault()`）。下の層は
 *    `event.defaultPrevented` を見て譲る。
 * 2. **下の層は、可視で活性な「上位層」が出ているあいだ Esc に反応しない**
 *    （{@link hasVisibleLayerAbove}）。**上位＝自分より z-index が大きい層**
 *    （#381 レビュー対応14回目。それ以前は「自分以外の可視な層」だったので、
 *    パレット(1000)と Drawer(900) が重なると互いを上と誤認して両方が譲り、
 *    どちらも反応しない/トラップが両方とも引き戻さない、という穴があった）。
 *    `except` を渡さない呼び出し（自分は層ではない＝最下層のツリー・サイドバー）
 *    は従来どおり「可視で活性な層が1つでもあれば真」。`event.target` を見るだけ
 *    では足りない:
 *    `Drawer.svelte` はタブ移動を閉じ込めない（同ファイル冒頭 doc）ので、
 *    ダイアログが出たままフォーカスだけが下の層に戻っていることがあり、その
 *    Esc は「発生元が上位層の中ではない」ので素通りしてしまう。
 *
 *    **これはページ側の window Esc ハンドラにも当てはまる**（例: タグ登録の
 *    「一覧から挿入」トグル）。本文と同じ最下層に属するものが、上に層が出ている
 *    あいだの Esc を食べてはいけない。
 *
 * 1 だけに頼れないのは、window に張った Esc ハンドラ同士では
 * `stopPropagation` が効かず、どちらが先に走るかがリスナーの登録順しだいに
 * なるため（登録順は「いつ開いたか」で変わる）。2 があれば順序に依存しない。
 *
 * 3. **上位層は window レベルで Esc を処理する**（フォーカス位置に依存しない）。
 *    パネル内の要素の `onkeydown` だけで閉じていると、フォーカスがその層の外へ
 *    出た瞬間（項目4のトラップがあっても、プログラム的な `focus()` 等の外部要因で
 *    外れることはある）にその層は閉じられ
 *    なくなり、一方で下の層は 2 によって全員「上に層がある」と譲るので、
 *    **Esc を押しても何も閉じない**状態になる（`CommandPalette.svelte` で実際に
 *    起きていた）。**メニュー（`TreeContextMenu`）はさらに、フォーカスが外へ出たら
 *    閉じる**（`focusout`）- 「見えているのにフォーカスは外」という状態自体を
 *    作らないのが一番の対策で、window Esc はその保険。
 * 4. **モーダル層（`Drawer`/`Modal`/`CommandPalette`）はフォーカストラップで裏の
 *    起動経路を断つ**（`focusTrap.ts`）。フォーカスがパネルの外へ出られると、
 *    オーバーレイの裏にある起動ボタンへ Tab で届いて別の層を開けてしまい、
 *    重なりの前提（1本の積み重ね）が崩れる。**`aria-modal` を名乗る層は必ず
 *    トラップを持つこと**（名乗るだけで閉じ込めないのは支援技術への嘘にもなる）。
 *    トラップは**パネルの `keydown`（Tab の循環）+ document の `focusin`（外へ
 *    着地したフォーカスの引き戻し）の2段構え**にする（`focusTrap.ts`）: 一度外へ
 *    出てしまうと以後の Tab はパネルの外で起きるので、パネルの keydown だけでは
 *    復帰できない。
 * 5. **層を閉じたら開いた元へフォーカスを戻す**（`focusRestore.ts`）。`<body>` に
 *    落とすと、そこからの Tab は**どのパネルの keydown も通らない**ので
 *    項目4のトラップを全部すり抜ける（閉じ残った層があれば、下の層は項目2で
 *    譲り続けるので Esc も効かなくなる）。戻し先が消えている / `inert` の中なら
 *    呼び出し側が決めた代替へ（無ければ何もしない）。
 * 6. **退避中・閉じ遷移中の層は「無い」ことを DOM で明示する**: `transform` で
 *    画面外へ逃がしただけでは矩形も `visibility` も残るので、この判定にも
 *    `focusRestore.ts` の生存判定にも**可視な層として引っかかり続ける**。退避中は
 *    `visibility: hidden` + `inert`（`SplitPane` の退避ペイン・`Sidebar` の
 *    オフキャンバス）、outro（fade/fly）のあいだ DOM に残る Drawer/Modal は
 *    {@link LAYER_INACTIVE_ATTR} を付けて、どちらもここから除外する。
 * 7. **非モーダル層（サイドバー・退避ツリー）は、下位を開くときに上位を先に畳む**
 *    （`tags`/`monitor` の `toggleTree`）。こちらはトラップを張れない（画面の
 *    一部としてそのまま操作できることが目的の UI）ので、順序は開く側が揃える。
 *
 * ## 層（z-index、大きいほど手前）
 *
 * | 層                                          | z-index | role                      |
 * | ------------------------------------------- | ------- | ------------------------- |
 * | `CommandPalette.svelte` / `ToastHost.svelte` / `TreeContextMenu.svelte` | 1000 | `dialog` / -（トースト）/ `menu` |
 * | `Drawer.svelte` / `Modal.svelte`            | 900     | `dialog`                  |
 * | オフキャンバスサイドバー（`Sidebar.svelte`、バックドロップ 700） | 710 | -（常設ナビ）    |
 * | 狭幅の退避ツリー（`SplitPane.svelte`、バックドロップ 600） | 610 | `region`         |
 *
 * **同じ z 順の層を同時に出さないのは呼び出し側の責務**: この判定は role と可視性
 * しか見ないので、同じ層の2つ（例: 接続 Drawer と収集グループ Drawer、どちらも
 * 900）を同時に開くと互いを「手前の層」と見なして**どちらも Esc で閉じなくなる**。
 * 元々同時に出さない設計なので、**相手が開いているあいだは開かないこと**
 * （`tags/+page.svelte::blockedByOtherResourceDrawer`）。相手を閉じる側に倒すと、
 * 相手の未保存入力を黙って捨てることになる（#376 で塞いだ事故と同じ）。
 * **コマンドパレットとコンテキストメニューも同じ z（1000）**なので同時に出さない:
 * メニューが出ているあいだ `Ctrl+K` はパレットを開かない
 * （`(app)/+layout.svelte`。メニューは一過性で、レイアウトからページのメニューを
 * 閉じる口が無いため「開かない」側に倒した - Esc で閉じてから開く）。
 *
 * {@link LAYER_ABOVE_SELECTOR} が拾うのは **Esc で閉じる一時的な UI**
 * （`dialog` = Drawer/Modal/CommandPalette、`menu` = TreeContextMenu）だけ。
 * トーストは Esc で閉じないので入れない。サイドバーは role を名乗らない常設
 * ナビなのでセレクタでは拾えず、代わりに約束1（閉じるときに消費する）を
 * `(app)/+layout.svelte` 側が守る - それより下の層は退避ツリーだけなので
 * これで足りる。新しく重なる UI を足すときは、この表と下のセレクタを更新すること。
 */

/**
 * 閉じ始めた層に付ける印（#381 レビュー対応12回目、約束6）。`Drawer`/`Modal` は
 * `open=false` になっても outro（fade/fly）のあいだ DOM に残り、矩形も
 * `visibility` も「可視」のままなので、そのままだと {@link hasVisibleLayerAbove}
 * が ~150ms のあいだ true を返し続け、その間の Esc は「上に層がある」として
 * 下の層が全員譲るのに閉じるものが無い＝**無反応**になる。
 */
export const LAYER_INACTIVE_ATTR = 'data-layer-inactive';

/** Esc で閉じる一時的な上位層が名乗る role（上の表を参照）。 */
export const LAYER_ABOVE_SELECTOR = '[role="dialog"], [role="menu"]';

/** `[role="menu"]` の層（コンテキストメニュー）だけを指すセレクタ。 */
export const MENU_LAYER_SELECTOR = '[role="menu"]';

/**
 * 要素の実効 z-index: 自身から positioned な祖先へ辿って**最初に見つかった
 * 数値**を返す（`auto` や static は飛ばす）。どこにも無ければ `0`。
 *
 * パネル自身（`.drawer`/`.modal`）は positioned ではなく、z-index を持つのは
 * それを包むオーバーレイなので、この「辿る」振る舞いが要る（値は CSS の
 * 定義をそのまま読む - この表の数値をコードへ写さない）。
 */
export function effectiveZIndex(el: Element): number {
	let node: Element | null = el;
	while (node) {
		const style = getComputedStyle(node);
		const z = Number.parseInt(style.zIndex, 10);
		if (!Number.isNaN(z) && style.position !== 'static') return z;
		node = node.parentElement;
	}
	return 0;
}

/** 可視で活性（閉じ遷移中でない）な層か。 */
function isActiveLayer(el: Element): boolean {
	if (el.hasAttribute(LAYER_INACTIVE_ATTR)) return false;
	if (el.getClientRects().length === 0) return false;
	const style = getComputedStyle(el);
	return style.display !== 'none' && style.visibility !== 'hidden';
}

/** 可視で活性な `[role="menu"]` が出ているか（`Ctrl+K` の抑止に使う）。 */
export function hasVisibleMenuLayer(): boolean {
	for (const el of document.querySelectorAll(MENU_LAYER_SELECTOR)) {
		if (isActiveLayer(el)) return true;
	}
	return false;
}

/**
 * いま**可視な**上位層（{@link LAYER_ABOVE_SELECTOR}）が出ているか。
 *
 * 閉じている Drawer/Modal/メニューは `{#if open}` で DOM ごと消えるが、
 * `display: none` や `visibility: hidden` で閉じるものが将来混じっても
 * 誤検出しないよう、矩形の有無と計算済みスタイルの両方で可視性を見る。
 *
 * `except` に**自分自身の要素**（層を名乗る要素そのもの）を渡すと、**自分より
 * z-index が大きい層だけ**を数える（#381 レビュー対応14回目。それ以前は「自分
 * 以外の可視な層」だったため、パレット(1000)と Drawer(900) が重なると互いを
 * 「上」と誤認し、Esc もフォーカスの引き戻しも双方が譲って**誰も反応しない**
 * 状態になっていた）。自分自身とそれを包む層は数えない。
 *
 * `except` を渡さない場合は従来どおり「可視で活性な層が1つでもあるか」
 * （呼び出し側が層ではない＝最下層のツリー・サイドバー・ページ側ハンドラ）。
 */
export function hasVisibleLayerAbove(options: { except?: Element | null } = {}): boolean {
	const except = options.except ?? null;
	const exceptZ = except ? effectiveZIndex(except) : null;
	for (const el of document.querySelectorAll(LAYER_ABOVE_SELECTOR)) {
		if (except && (el === except || el.contains(except))) continue;
		if (!isActiveLayer(el)) continue;
		if (exceptZ !== null && effectiveZIndex(el) <= exceptZ) continue;
		return true;
	}
	return false;
}
