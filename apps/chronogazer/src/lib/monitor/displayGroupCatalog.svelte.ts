/**
 * 表示グループの一覧（監視画面とコマンドパレットで共有する、R1-D）。
 *
 * コマンドパレットの「グループ: ◯◯ を表示」は、パレットを開いた画面に
 * かかわらず出したいので、一覧は画面ではなくこのモジュールのシングルトンに
 * 置く（`commandPalette.svelte.ts` と同じ形）。監視画面が読むたびに、パレットの
 * コマンドも同じ一覧に追従する。
 *
 * - **読めていない（`null`）と 0 件（`[]`）を分ける**（チェックリスト §5）。
 *   読み直しに失敗しても、前に読めた一覧は消さない（`error` を添えるだけ）。
 * - 同時に呼ばれた読み込みは 1 本にまとめる（監視画面とパレットが同時に読む）。
 * - 一覧は表示の並び（`sortOrder`）に並べてから持つ。
 */
import { isProviderError } from '@banto/admin-core';
import { listDisplayGroups, type DisplayGroup } from '../banto/displayGroupsAdmin';
import { sortDisplayGroups } from './monitorLogic';

export type DisplayGroupLoader = () => Promise<DisplayGroup[]>;

export class DisplayGroupCatalog {
	/** 並べ替え済みの一覧。まだ一度も読めていなければ `null`。 */
	groups = $state<DisplayGroup[] | null>(null);
	/** 最後の読み込みの失敗（成功で `null`）。 */
	error = $state<string | null>(null);
	loading = $state(false);

	readonly #load: DisplayGroupLoader;
	#inflight: Promise<void> | null = null;

	constructor(load: DisplayGroupLoader = listDisplayGroups) {
		this.#load = load;
	}

	/** 読み直す。読み込み中なら、その読み込みの完了を待つ（2 本目を投げない）。 */
	refresh(): Promise<void> {
		if (this.#inflight) return this.#inflight;
		this.loading = true;
		const run = (async () => {
			try {
				this.groups = sortDisplayGroups(await this.#load());
				this.error = null;
			} catch (err) {
				this.error = isProviderError(err) ? err.message : String(err);
			}
		})();
		// 片付けは必ず非同期で走らせる（読み込みが同期で投げても、`#inflight` を
		// 設定する前に片付けが終わって詰まらないように）。
		const settled = run.finally(() => {
			this.loading = false;
			this.#inflight = null;
		});
		this.#inflight = settled;
		return settled;
	}
}

export const displayGroupCatalog = new DisplayGroupCatalog();
