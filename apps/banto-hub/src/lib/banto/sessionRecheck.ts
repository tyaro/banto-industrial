/**
 * ストリームが `1008` + `session_revoked` / `commissioning_ended` で閉じられた
 * ときに、ログイン状態を確かめ直す（#441）。
 *
 * 新しい判断は持たない: `invalidateAll()` で `(app)/+layout.ts` のルート
 * ガードを走らせ直すだけ。ガードは画面を開いたときと同じ経路
 * （`fetchCommissioningStatusOrNull` → `decideProtectedRoute` =
 * `resolveProtectedSession`、#436）で判断する:
 *
 * - 失効を確認できた（`401` など）→ `/login` へ移る
 * - 照合できなかった（`500`・到達不能）→ 再試行付きのエラー画面
 *   （`routes/+error.svelte`）。保存しているトークンは消さない
 * - まだ有効 → 画面はそのまま（呼び出し側がストリームを再開する）
 * - `commissioning_ended`: 試運転モードではなくなったので、ガードは
 *   ログインを求める側へ進む（トークンが無ければ `/login`）
 *
 * {@link createSingleFlight} で包み、複数のストリームが同時に閉じられても
 * 確認（= 画面の移動・エラー表示）は 1 回にする。
 */
import { invalidateAll } from '$app/navigation';
import { createSingleFlight } from './streamClose';

export const recheckSessionAfterStreamClose: () => Promise<void> = createSingleFlight(() =>
	invalidateAll()
);
