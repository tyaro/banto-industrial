/**
 * 低い画面では、下のログのパネル（追加パネル `log`）を下端の自動で隠す帯で始める（#1253、
 * 2026-11-21 オーナー決定。docs/hub/banto-hub-ui-improvement-proposal.md §下のログのパネル）。
 * `/tags` が使う（もとは `/monitor` と共用。#1255 段階 ③で `/monitor` は消えた）。
 *
 * 下の区画の高さは部品の決め打ち（枠の高さの 25%）なので、1280×720 や 1366×768・125%
 * （CSS で約 1093×614）の既定の配置では、`/tags` のタグ一覧に行が 1 行も出ない。
 *
 * - **既定の配置を部品が作ったときだけ**当てる: 保存値が無い・壊れていた（部品の `onRestore` が
 *   `fresh`・`default`）、または「レイアウトを既定に戻す」。利用者が変えた配置（保存値）は
 *   そのまま従い、高さで畳み直さない（幅の狭い表示とは別の扱い）。
 * - 高さは**画面（ビューポートの CSS の高さ、`innerHeight`）**で測る。枠の高さはヘッダーや
 *   ツールバーの分だけページごとに違うので、2 つのページで同じ判定にするため画面で測る。
 * - 帯にするのは部品の口 `DockState.autoHideRegion('log', { edge: 'bottom' })`（聞かずに動かす。
 *   ピンを外したのと同じ形で、元の場所を覚えるので、帯のピンで下のドッキングへ戻せる）。
 * - 最初の既定: 保存の口の「最初の save は基準にするだけ」を守るため、保存の基準を取る直前
 *   （`persistThreePaneLayout` の `beforeBaseline`）に帯にする。帯で始めた既定を利用者の変更と
 *   して書かない。
 * - 狭い表示（部品が追加パネルを隠す）の間はログが木に無いので、`dock` では帯にできない。部品が
 *   控えている広い配置（`layoutSnapshot()`。狭い表示の間はこれが保存の対象）に帯の補正をかけ、
 *   `restoreLayout(補正後)` で控えを置き換える（部品の README「狭い表示」: 狭い表示の間の
 *   `restoreLayout` は控えを置き換える）。補正は控えそのものに当たるので、狭 → 広の切り替えの
 *   時点で新たな変更として起きない（PR #1270 レビュー P2）。置き換えが済むまでは `holding()` が
 *   真で、保存の口は基準も変更も取らない（基準は補正後の控え）。
 *   代わりに、部品が控えの既定を広い幅で測り直す（`wideIsDefault`）のは効かなくなり、左右の幅は
 *   置き換えた時点の枠の幅で測った割合のままになる。両ページは狭い表示を `narrowOverride` で
 *   最初から出すので、そのときの既定は部品の標準の幅（1280px）で測ってある。
 * - 「既定に戻す」: 部品の `resetLayout` の後に帯にする（利用者の操作なので、その結果は保存する）。
 *   狭い表示なら上と同じく控えを置き換える。済むまで保存を止め、補正前の配置を書かない。
 * - サーバーの保存値（`restoreSaved`）: 部品の `restoreLayout` の結果の `status` で、保存値を採った
 *   （`restored`・`repaired`）か、外形だけ正しく中身が使えずに部品が既定へ戻した（`default`）かを
 *   分ける（PR #1270 再レビュー P2。例 `{"schema":1,"panels":[],"layout":{}}` は `dockLayout.ts` の
 *   外形の検証を通るが、部品は開いた既定を当てて `default` を返し、`onRestore` は呼ばない）。
 *   `default` なら最初の既定と同じ扱い: 低い画面では帯にし、利用者の変更としては書かない
 *   （復元から補正まで `holding()` で保存を止め、済んだ配置を `rebaseline()` で保存の基準に
 *   置き直す）。採ったときは今どおり補正しない（利用者の配置に従う）。
 */
import { createDockState, type DockLayout, type ThreePaneViewMenuItem } from '@banto/dock-svelte';

/**
 * この高さ（CSS px）未満の画面では、既定の配置でログを帯で始める。1280×720・1366×768 の 125%
 * （614）・1280×800 では帯、1920×1080 では開く。900 はその間で、下の 25% を取っても中央に
 * 一覧の行が十分に残る高さの目安（オーナーの目安の値）。
 */
export const LOG_BAND_MAX_VIEWPORT_HEIGHT = 900;

/** 下のログのパネルの id（両ページの追加パネル）。 */
export const LOG_PANEL_ID = 'log';

/**
 * 部品が最初の配置・復元の結果を既定で作ったか（`onRestore` の `fresh`・`default`、
 * `restoreLayout` の `status` の `default`）。保存値を採った `restored`・`repaired` は偽。
 */
export function builtFromDefault(status: string | undefined): boolean {
	return status === 'fresh' || status === 'default';
}

/** その高さの画面で、既定の配置のログを帯で始めるか。高さが分からない（0 以下）なら始めない。 */
export function startsLogInBand(viewportHeight: number): boolean {
	return viewportHeight > 0 && viewportHeight < LOG_BAND_MAX_VIEWPORT_HEIGHT;
}

function currentViewportHeight(): number {
	return typeof window === 'undefined' ? 0 : window.innerHeight;
}

/** ドッキング中のログを下端の帯へ移す（部品の口）。木に無ければ何もしない。移したか。 */
export function bandLogPanel(dock: {
	autoHideRegion(id: string, options?: object): boolean;
}): boolean {
	return dock.autoHideRegion(LOG_PANEL_ID, { edge: 'bottom' });
}

/**
 * 部品の既定の配置（`threePaneDockLayout` の結果）を、画面の高さで直した既定の配置。低ければ
 * ログを帯にした配置、そうでなければそのまま（ページと同じ部品の口を一時の `DockState` で使う）。
 */
export function lowScreenDefaultLayout(layout: DockLayout, viewportHeight: number): DockLayout {
	if (!startsLogInBand(viewportHeight)) return layout;
	const temp = createDockState(layout);
	return bandLogPanel(temp) ? $state.snapshot(temp.layout) : layout;
}

/** 部品（`ThreePaneDock`）の口のうち、ここで使うもの。 */
export interface LogBandPane {
	resetLayout(): Promise<string>;
	/** 保存の対象（狭い表示の間は控えた広い配置）。 */
	layoutSnapshot(): { schema: 1; panels: string[]; layout: object };
	/**
	 * 保存値を当てる（狭い表示の間は控えを置き換える）。`status` は部品の `ThreePaneRestoreStatus`
	 * （`restored`・`repaired`・`default`）。
	 */
	restoreLayout(saved: unknown): Promise<{ status?: string; outcome: string }>;
}

export interface LowScreenLogBandOptions {
	pane: () => LogBandPane | undefined;
	dock: { autoHideRegion(id: string, options?: object): boolean };
	/** 部品の `bind:narrow`（狭い表示を出しているか）。 */
	narrow: () => boolean;
	/** 既定は `window.innerHeight`（試験で差し替える）。 */
	viewportHeight?: () => number;
}

export interface LowScreenLogBand {
	/** 部品の `onRestore` に渡す（最初の配置を部品が既定で作ったか）。 */
	onRestore(status: string): void;
	/** `persistThreePaneLayout` の `beforeBaseline` に渡す（最初の保存の基準の直前に 1 回）。 */
	beforeBaseline(): void;
	/** 補正（控えの置き換え・既定に戻す）の途中か。真の間は保存しない（反応する値）。 */
	holding(): boolean;
	/**
	 * サーバーの保存値を当てる（以後、途中の補正の結果に手を加えない）。部品が既定へ戻した
	 * （`status: 'default'`）なら、その既定にも低い画面の補正を当て、保存しない（基準に置き直す）。
	 */
	restoreSaved(pane: LogBandPane, saved: unknown): void;
	/**
	 * 保存を止めていた補正の結果を、利用者の変更ではなく保存の基準として扱うか（1 回だけ真を返す）。
	 * `persistThreePaneLayout` が `holding()` の明けに読む。
	 */
	rebaseline(): boolean;
	/** 「表示」メニューの項目の「レイアウトを既定に戻す」を、この規則の既定に戻す形へ替える。 */
	viewItems(items: ThreePaneViewMenuItem[]): ThreePaneViewMenuItem[];
}

/** コンポーネントの初期化中に呼ぶ。 */
export function lowScreenLogBand(options: LowScreenLogBandOptions): LowScreenLogBand {
	const height = options.viewportHeight ?? currentViewportHeight;
	let startedDefault = false;
	/** サーバーの保存値を当て始めたか（以後、最初の基準の補正はしない。復元の側が補正する）。 */
	let savedTaken = false;
	/** 済んだ補正の配置を、保存の基準に置き直すか（`rebaseline()` で 1 回だけ読む）。 */
	let rebase = false;
	/** 途中の補正の数（0 で保存を戻す）。 */
	let pending = $state(0);
	/** 最新の補正の世代（サーバーの保存値・新しい「既定に戻す」で古い補正を見送る）。 */
	let generation = 0;

	/**
	 * 低い画面なら、いまの既定のログを帯にする。ドッキングしていれば `dock` で動かし、狭い表示で
	 * 木に無ければ、控えた広い配置を補正して置き換える（済むまで `holding()`）。
	 */
	async function bandIfLow(pane: LogBandPane, gen: number): Promise<void> {
		if (!startsLogInBand(height())) return;
		if (bandLogPanel(options.dock)) return;
		if (!options.narrow()) return;
		const kept = pane.layoutSnapshot();
		const banded = lowScreenDefaultLayout(kept.layout as DockLayout, height());
		if (banded === kept.layout || gen !== generation || options.pane() !== pane) return;
		await pane.restoreLayout({ ...kept, layout: banded });
	}

	async function held(run: () => Promise<void>): Promise<void> {
		pending += 1;
		try {
			await run();
		} finally {
			pending -= 1;
		}
	}

	function reset(): void {
		const pane = options.pane();
		if (!pane) return;
		const gen = ++generation;
		void held(async () => {
			const outcome = await pane.resetLayout();
			if (outcome !== 'applied' || options.pane() !== pane || gen !== generation) return;
			await bandIfLow(pane, gen);
		});
	}

	return {
		onRestore(status) {
			startedDefault = builtFromDefault(status);
		},
		beforeBaseline() {
			const pane = options.pane();
			if (!startedDefault || savedTaken || !pane) return;
			const gen = ++generation;
			void held(() => bandIfLow(pane, gen));
		},
		holding() {
			return pending > 0;
		},
		restoreSaved(pane, saved) {
			savedTaken = true;
			const gen = ++generation;
			void held(async () => {
				const result = await pane.restoreLayout(saved);
				if (result.outcome !== 'applied' || !builtFromDefault(result.status)) return;
				if (gen !== generation || options.pane() !== pane) return;
				await bandIfLow(pane, gen);
				if (gen === generation) rebase = true;
			});
		},
		rebaseline() {
			const taken = rebase;
			rebase = false;
			return taken;
		},
		viewItems(items) {
			return items.map((item) =>
				item.kind === 'action' && item.id === 'reset-layout'
					? { ...item, onSelect: () => reset() }
					: item
			);
		}
	};
}
