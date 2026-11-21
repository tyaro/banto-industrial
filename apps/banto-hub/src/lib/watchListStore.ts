/**
 * 見張っているタグの一覧を利用者ごとに保存する小さな口（#1254。純粋な部分は `watchList.ts`）。
 * 置き場と約束は `dockLayoutStore.ts` と同じ二層:
 * - localStorage（`banto.watch.v1.<持ち主>`）: 写し。最初の描画に使う。書けなくても黙って続ける。
 *   持ち主は作った時点で決め、分からなければ写しを読まず書きもしない。作った後に持ち主が
 *   替わっていたら写しを書かない。
 * - `UiSettingsProvider`（`hub.watch.v1`）: 本体。デスクトップ版は起動ごとに origin が変わり
 *   localStorage が消えるので、マウント後に `loadSaved()` で読み直して当てる。送信は
 *   `createSavedSetting`（即時の形）に任せる。保存済みの印は送信が成功した値だけ。
 *
 * 呼び出し側の約束:
 * - 最初の `save()` は、マウントした時点の一覧（写し）で利用者の変更ではない（書かずに基準にする）。
 * - サーバーの値は `loadSaved()` が返す。読み終わる前に利用者が一覧を変えていたら返さず、
 *   その変更をサーバーへ送る（利用者の最新の操作が勝つ）。
 */
import {
	createSavedSetting,
	getSessionController,
	type UiSettingsProvider
} from '@banto/admin-core';
import { getUiSettings } from './banto/setup';
import { parseWatchList, serializeWatchList, watchListKeys } from './watchList';

export interface WatchListStore {
	/** 最初の描画に使う写し。無い・壊れていれば空。 */
	readonly initial: number[];
	/** 今の一覧を保存する（同じ一覧なら何もしない）。 */
	save(list: readonly number[]): void;
	/** サーバーの保存値を読む。当てるべき値があるときだけ返す。 */
	loadSaved(): Promise<number[] | undefined>;
	/** ページを離れるときに呼ぶ。 */
	dispose(): void;
}

export interface WatchListStoreDeps {
	provider?: UiSettingsProvider;
	storage?: () => Pick<Storage, 'getItem' | 'setItem'> | undefined;
	owner?: () => string | null;
}

function defaultOwner(): string | null {
	try {
		const snapshot = getSessionController().snapshot;
		return snapshot.status === 'active' ? snapshot.owner : null;
	} catch {
		return null;
	}
}

function defaultStorage(): Pick<Storage, 'getItem' | 'setItem'> | undefined {
	try {
		return typeof localStorage === 'undefined' ? undefined : localStorage;
	} catch {
		return undefined;
	}
}

export function createWatchListStore(deps: WatchListStoreDeps = {}): WatchListStore {
	const currentOwner = deps.owner ?? defaultOwner;
	const owner = currentOwner();
	const keys = watchListKeys(owner);
	const storage = deps.storage ?? defaultStorage;
	const setting = createSavedSetting<number[] | null>({
		provider: deps.provider ?? getUiSettings(),
		key: keys.setting,
		parse: (raw) => parseWatchList(raw) ?? null,
		serialize: (value) => (value === null ? '' : serializeWatchList(value)),
		fallback: () => null,
		mode: 'immediate'
	});

	function readLocal(): number[] {
		if (keys.local === undefined) return [];
		try {
			return parseWatchList(storage()?.getItem(keys.local)) ?? [];
		} catch {
			return [];
		}
	}

	function writeLocal(raw: string): void {
		if (keys.local === undefined || currentOwner() !== owner) return;
		try {
			storage()?.setItem(keys.local, raw);
		} catch {
			// 写しは best-effort（本体はサーバー）。
		}
	}

	const initial = readLocal();
	/** 最後に受け取った一覧（`undefined` = まだマウント時の基準を受けていない）。 */
	let lastRaw: string | undefined;
	let loaded = false;
	let pending: number[] | undefined;
	let disposed = false;

	return {
		initial,
		save(list) {
			if (disposed) return;
			const raw = serializeWatchList(list);
			if (lastRaw === undefined) {
				lastRaw = raw;
				return;
			}
			if (raw === lastRaw) return;
			lastRaw = raw;
			writeLocal(raw);
			if (loaded) setting.set([...list]);
			else pending = [...list];
		},
		async loadSaved() {
			const outcome = await setting.load();
			if (disposed) return undefined;
			loaded = true;
			if (pending !== undefined) {
				const latest = pending;
				pending = undefined;
				setting.set(latest);
				return undefined;
			}
			if (outcome !== 'loaded') return undefined;
			const saved = setting.saved;
			if (saved === null || saved === undefined) return undefined;
			const raw = serializeWatchList(saved);
			if (raw === lastRaw) return undefined;
			// 当てる値は利用者の変更ではない: 基準にして（サーバーへ送り返さない）写しだけ更新する。
			lastRaw = raw;
			writeLocal(raw);
			return saved;
		},
		dispose() {
			disposed = true;
			setting.dispose();
		}
	};
}
