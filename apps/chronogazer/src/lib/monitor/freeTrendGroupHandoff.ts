/**
 * 監視の画面の「この組を自由トレンドとして保存」から自由トレンドの画面への引き継ぎ（8c・オーナーのレビューの P2）。
 *
 * 下書き本体は sessionStorage に書いて `/free-trends?new=1` へ移る。URL だけだと、移った先で一覧を読む間に
 * ログインし直すなどでセッションが替わっても、引き継いだ下書きを照合しないまま編集欄を開いてしまう。
 * そこで移る直前に「持ち主・セッションの世代・写せなかった本数」をこのタブのメモリに置き、受け側が
 * 今の持ち主・世代・下書きの有無と照合して開くか捨てるかを決める。メモリだけなので再読み込みで消えてよい
 * （そのときは `none` で何もしない）。
 * 照合に合えば開く。セッションが替わった・下書きが無いなら `stale`（開かず知らせる）。作れない・上限のときは
 * `blocked`（知らせない。上限の注記が画面に出ていて、下書きは残り、後で新規の下書きから続けられる）。
 */

export type GroupHandoff = { owner: string; generation: number; skipped: number };

let slot: GroupHandoff | null = null;

/** 引き継ぎの印を置く（前のものは捨てる）。 */
export function offerGroupHandoff(h: GroupHandoff): void {
	slot = h;
}

/** 引き継ぎの印を取り出して空にする。 */
export function takeGroupHandoff(): GroupHandoff | null {
	const h = slot;
	slot = null;
	return h;
}

export type GroupHandoffNow = {
	owner: string | null;
	generation: number;
	hasDraft: boolean;
	canCreate: boolean;
	createBlocked: boolean;
};

/** `open`: 開く。`stale`: セッション・下書きが合わず開かない（知らせる）。`blocked`: 作れない・上限（知らせない）。`none`: 引き継ぎ無し。 */
export function groupHandoffDecision(
	handoff: GroupHandoff | null,
	now: GroupHandoffNow
): 'open' | 'stale' | 'blocked' | 'none' {
	if (handoff === null) return 'none';
	if (handoff.owner !== now.owner || handoff.generation !== now.generation || !now.hasDraft) {
		return 'stale';
	}
	if (!now.canCreate || now.createBlocked) return 'blocked';
	return 'open';
}
