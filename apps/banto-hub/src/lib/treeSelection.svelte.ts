/**
 * #1179 PR3b（docs/hub/banto-hub-ui-improvement-proposal.md §HUB への当てはめ、段階 ①
 * 「2 つのページでツリーの選択を共有する」）: タグ登録（`/tags`）とタグモニタ
 * （`/monitor`）で最後に選んだツリーの対象。
 *
 * - **書くのは 2 つのページ**（URL から決めた・操作で変えた対象を、そのつど
 *   `set`）。**読むのはナビのリンク**（`navItemHref`、`#lib/navigation.ts`）だけで、
 *   ページはこれを読まない - 選択を決めるのは常に URL（`?target=`）。ストアから
 *   ページの状態を決めると、戻る・進むで来た「すべて」の項目（`target` 無し）を
 *   ストアの値で上書きしてしまう。
 * - **保持はセッションの間のメモリだけ**: 再読み込みでは今のページの URL が
 *   対象を持っているので、読み込み直後にページがここへ書き戻す。もう一方の
 *   ページの選択は同じなので、再読み込みをまたぐ保存（`sessionStorage`）は要らない。
 * - 消えた対象（別の画面で削除した接続・グループ）が残っていることがある。その
 *   ときは移った先のページが「見つからない」案内を出して「すべて」に倒す
 *   （URL で開いたときと同じ）。
 */
import { sameTreeTarget } from './banto/tagsView';
import type { TreeFilterSelection } from './banto/treeFilterPrune';

class SharedTreeTarget {
	#current: TreeFilterSelection = $state({ type: 'all' });

	get current(): TreeFilterSelection {
		return this.#current;
	}

	set(target: TreeFilterSelection): void {
		if (!sameTreeTarget(this.#current, target)) this.#current = target;
	}
}

export const sharedTreeTarget = new SharedTreeTarget();
