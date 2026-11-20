/**
 * 自由トレンドの画面（#1207・#1231 PR 8b1）の純粋な判断。監視画面の `?group=`（`monitorLogic.ts` の
 * `parseGroupParam`・`selectGroup`・`loadLastGroup`）と同じ作法を、`?id=` と別のキーで持つ。
 */
import { parseGroupParam } from './monitorLogic';

/** URL の `?id=` の値を ID に読む。数字でなければ `null`。 */
export const parseFreeTrendParam = parseGroupParam;

/** 表示する自由トレンドの選び方の結果。 */
export interface FreeTrendSelection {
	/** 表示する自由トレンドの ID（一覧が空なら `null`）。 */
	id: number | null;
	/** URL で指定されたものが一覧に無かった（消された・書き間違い）。画面が断る。 */
	missingRequested: number | null;
}

/**
 * 表示する自由トレンドを決める（純関数）。優先順位は URL（`?id=`）→ この端末で最後に見たもの →
 * 一覧の先頭。どれも一覧に無いものは採らない。
 */
export function selectFreeTrend(
	list: readonly { id: number }[],
	requested: number | null,
	remembered: number | null
): FreeTrendSelection {
	const exists = (id: number | null): id is number =>
		id !== null && list.some((item) => item.id === id);
	if (exists(requested)) return { id: requested, missingRequested: null };
	const missingRequested = requested;
	if (exists(remembered)) return { id: remembered, missingRequested };
	return { id: list[0]?.id ?? null, missingRequested };
}

export const LAST_FREE_TREND_STORAGE_KEY = 'chronogazer.freeTrends.last';

/** 端末に覚えた最後の自由トレンド。ストレージが使えない・壊れていれば `null`。 */
export function loadLastFreeTrend(storage: Pick<Storage, 'getItem'> | undefined): number | null {
	if (!storage) return null;
	try {
		return parseFreeTrendParam(storage.getItem(LAST_FREE_TREND_STORAGE_KEY));
	} catch {
		return null;
	}
}

/** 覚える（ベストエフォート）。 */
export function saveLastFreeTrend(storage: Pick<Storage, 'setItem'> | undefined, id: number): void {
	if (!storage) return;
	try {
		storage.setItem(LAST_FREE_TREND_STORAGE_KEY, String(id));
	} catch {
		// 端末の便利機能なので、失敗しても表示は続ける。
	}
}

/** 削除の完了後にすること。 */
export interface FreeTrendDeleteFollowUp {
	/** URL の `?id=` を外す（replace の遷移）。 */
	clearUrl: boolean;
	/** 一覧を読み直す。 */
	reload: boolean;
}

/**
 * 削除の応答を待つ間に画面が破棄された・別の自由トレンドが選ばれた場合に、後から来た完了が
 * 遷移や選択を上書きしないための判断（純関数）。破棄後は何もしない。URL は、その時点の `?id=`
 * が削除した ID を指したままのときだけ整理する。
 */
export function afterFreeTrendDelete(args: {
	destroyed: boolean;
	currentRequestedId: number | null;
	deletedId: number;
}): FreeTrendDeleteFollowUp {
	if (args.destroyed) return { clearUrl: false, reload: false };
	return { clearUrl: args.currentRequestedId === args.deletedId, reload: true };
}

/** 画面で選び直した時間窓（どの自由トレンドのものか持つ）。 */
export interface FreeTrendWindowChoice {
	id: number;
	sec: number;
}

/** 表示中の自由トレンドが変わったら、選び直した時間窓を捨てる（保存値に戻る）。 */
export function windowChoiceAfterSelection(
	choice: FreeTrendWindowChoice | null,
	selectedId: number | null
): FreeTrendWindowChoice | null {
	return choice !== null && choice.id === selectedId ? choice : null;
}
