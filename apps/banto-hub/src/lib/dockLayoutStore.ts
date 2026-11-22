/**
 * ドックの枠の配置を利用者ごとに保存する小さな口（#1253。純粋な読み込みは `dockLayout.ts`）。
 * ページの名前でキーを分ける（もとは `/monitor` と `/tags` が使った。#1255 段階 ③で `/monitor` は消えた）。
 * banto のテンプレートの `dockLayoutStore.svelte.ts` と同じ役目で、パッケージの口にするよう
 * tyaro/banto#791 で提案した。入ったら置き換える。
 *
 * 置き場は `settings.svelte.ts` と同じ二層:
 * - localStorage（`banto.dock.v2.<page>.<持ち主>`）: 写し。最初の描画（`ThreePaneDock` の
 *   `initialLayout`）に使う。書けなくても（容量・無効）黙って続ける。持ち主（今のセッションの
 *   `owner`）は作った時点で決める。分からなければ写しを読まず（既定の配置）書きもしない。
 *   作った後に持ち主が替わっていたら（別のタブで別の利用者がログイン）、写しを書かない。
 * - `UiSettingsProvider`（`hub.dock.v2.<page>`、Hub のサーバー・利用者ごと）: 本体。デスクトップ版は
 *   起動ごとに origin が変わり localStorage が消えるので、マウント後に読み直して当てる。
 *   送信は `@banto/admin-core` の `createSavedSetting`（即時の形）に任せる: 送っている間は 1 本、
 *   その間の変更は最新だけを続けて送る。**保存済みの印は送信が成功した値だけ**で、失敗したら
 *   次の変更で最新を送り直す（dock-svelte の README「保存の約束」）。
 *
 * 呼び出し側の約束:
 * - 保存する値は `ThreePaneDock` の `layoutSnapshot()`（狭い表示の間も、控えた広い配置を返す）。
 *   `dock.serialize()` は狭い表示の一時の配置を含むので使わない。
 * - 最初の `save()` はマウントした時点の配置で、利用者の変更ではない（書かずに基準にする）。
 *   サーバーの値を読む前に既定の配置で上書きしないため。
 * - サーバーの値は `loadSaved()` が返す。読み終わる前に利用者が配置を変えていたら返さず、
 *   その変更をサーバーへ送る（利用者の最新の操作が勝つ）。
 */
import {
	createSavedSetting,
	getSessionController,
	type UiSettingsProvider
} from '@banto/admin-core';
import { getUiSettings } from './banto/setup';
import {
	dockLayoutKeys,
	parseStoredDockLayout,
	serializeDockLayout,
	type StoredDockLayout
} from './dockLayout';

export interface DockLayoutStore {
	/** 最初の描画に使う写し（localStorage）。無い・壊れていれば `undefined`（部品の既定）。 */
	readonly initial: StoredDockLayout | undefined;
	/** 今の配置を保存する（同じ配置なら何もしない）。 */
	save(layout: StoredDockLayout): void;
	/**
	 * 今の配置を、送らずに比べる基準に置き直す（利用者の変更ではない配置。サーバーの保存値が
	 * 使えず部品が既定へ戻したときの補正後の配置、`dockLogBand.svelte.ts`）。
	 */
	rebase(layout: StoredDockLayout): void;
	/** サーバーの保存値を読む。当てるべき値があるときだけ返す（上の約束）。 */
	loadSaved(): Promise<StoredDockLayout | undefined>;
	/** ページを離れるときに呼ぶ（送信中の応答を捨てる）。 */
	dispose(): void;
}

export interface DockLayoutStoreDeps {
	provider?: UiSettingsProvider;
	/** 既定は `globalThis.localStorage`（無い・使えないときは写しを使わない）。 */
	storage?: () => Pick<Storage, 'getItem' | 'setItem'> | undefined;
	/**
	 * 今のログインの持ち主（`SessionSnapshot.owner`）。既定は既定の `SessionController` の
	 * 確定したセッション（`active` でなければ `null`）。
	 */
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

export function createDockLayoutStore(
	page: string,
	deps: DockLayoutStoreDeps = {}
): DockLayoutStore {
	const currentOwner = deps.owner ?? defaultOwner;
	const owner = currentOwner();
	const keys = dockLayoutKeys(page, owner);
	const localKey = keys.local;
	const storage = deps.storage ?? defaultStorage;
	const setting = createSavedSetting<StoredDockLayout | null>({
		provider: deps.provider ?? getUiSettings(),
		key: keys.setting,
		parse: (raw) => parseStoredDockLayout(raw),
		serialize: (value) => (value === null ? '' : serializeDockLayout(value)),
		fallback: () => null,
		mode: 'immediate'
	});

	function readLocal(): StoredDockLayout | undefined {
		if (localKey === undefined) return undefined;
		try {
			return parseStoredDockLayout(storage()?.getItem(localKey));
		} catch {
			return undefined;
		}
	}

	function writeLocal(raw: string): void {
		if (localKey === undefined || currentOwner() !== owner) return;
		try {
			storage()?.setItem(localKey, raw);
		} catch {
			// 写しは best-effort（本体はサーバー）。
		}
	}

	/** 最後に受け取った配置（`undefined` = まだマウント時の基準を受けていない）。 */
	let lastRaw: string | undefined;
	/** サーバーの値を読み終えたか（読み終わる前の変更は `pending` に溜める）。 */
	let loaded = false;
	let pending: StoredDockLayout | undefined;
	let disposed = false;

	return {
		initial: readLocal(),
		save(layout) {
			if (disposed) return;
			const raw = serializeDockLayout(layout);
			if (lastRaw === undefined) {
				lastRaw = raw;
				return;
			}
			if (raw === lastRaw) return;
			lastRaw = raw;
			writeLocal(raw);
			if (loaded) setting.set(layout);
			else pending = layout;
		},
		rebase(layout) {
			if (disposed) return;
			lastRaw = serializeDockLayout(layout);
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
			return serializeDockLayout(saved) === lastRaw ? undefined : saved;
		},
		dispose() {
			disposed = true;
			setting.dispose();
		}
	};
}
