/**
 * 自由トレンドの下書きとセッションの境界（#1207・#1231 PR 8b2a のオーナー再レビュー P2、
 * docs/chronogazer/trend-design.md §10 の 8b2a）。
 *
 * 下書きのキーの持ち主は `account:<identity.id>` だが、ChronoGazer の `identity.id` は**ログイン名**で、
 * 不変のアカウント ID ではない（REST は `identity.username`、Tauri も同じ）。アカウントを退役させて
 * 同じ名前で作り直すと、キーの名前空間が同じになる。不変の ID は取れないので、名前の再利用そのものは
 * キーでは防げない。そこで**セッションの境界で、このタブの下書きを全部捨てる**:
 *
 * - 同じタブで別のアカウント（同じ名前の作り直しを含む）に替わるには、セッションが必ず
 *   「確定的に無効」を通る:
 *   - このタブのログアウト・失効（401）は `none` に確定する。
 *   - 別のタブのログイン・ログアウトは保存した資格情報を替えるので、セッションの制御
 *     （`@banto/admin-core` の SessionController）は `active` を `unknown` へ落とす（hold）。
 *     資格情報の変化でしか起きないので、`active` の後に見た `unknown` は境界として扱う。
 *   - ログイン画面はセッションが `none` のときに出る。
 * - **再読み込み直後の `unknown`**（このページでまだ何も見ていない・`active` を見ていない）は
 *   セッションを取り戻している途中なので捨てない。その後の `active` が印（前に有効だった持ち主）と
 *   同じなら残す（再読み込み・画面の移動で下書きが残る、8b2a の約束）。
 * - 閲覧公開・ログイン不要モードなど、持ち主の無い有効なセッションは下書きを持たないので捨てる。
 *
 * 残る穴: タブが閉じられずに止まっている間（ブラウザがタブを捨てて後で読み直す、など）に、別のタブで
 * 退役と同じ名前の作り直し・ログインが済むと、このタブは境界を見ずに同じ持ち主として戻る。
 * 不変のアカウント ID が identity に載るまでは防げない。
 *
 * あわせて**セッションの世代**を数える。有効なセッション（持ち主）が替わる・無効になるたびに進む。
 * 保存・削除・一覧の応答は、始めたときの世代と違えば UI にも保存領域にも反映しない。
 */
import type { SessionKind } from '@banto/admin-core';
import { DRAFT_STORAGE_PREFIX, draftOwner, type DraftStorage } from './freeTrendDraft';

/** 前に有効だった下書きの持ち主の印（sessionStorage）。再読み込みをまたいで持ち主を比べる。 */
export const DRAFT_OWNER_MARK_KEY = 'chronogazer.freeTrends.draftOwner';

/** 見張りが前に見たもの。 */
export type DraftSessionPhase =
	/** このページでまだ何も見ていない。 */
	| { kind: 'initial' }
	/** セッションを取り戻している途中（このページで `active` を見る前の `unknown`）。 */
	| { kind: 'loading' }
	/** 有効なセッション。`owner` は下書きの持ち主（持たないセッションは `null`）。 */
	| { kind: 'active'; owner: string | null }
	/** 確定的に無効（`none`、または `active` の後の `unknown`）。 */
	| { kind: 'ended' };

/** セッションの制御の snapshot のうち、判断に使うところ。 */
export interface DraftSessionSnapshot {
	status: 'unknown' | 'none' | 'active';
	kind: SessionKind | null;
	identity: { id?: unknown } | null;
}

/** snapshot から下書きの持ち主（`draftOwner` と同じ規則）。有効でなければ `null`。 */
export function draftOwnerOfSnapshot(snapshot: DraftSessionSnapshot): string | null {
	if (snapshot.status !== 'active') return null;
	const id = snapshot.identity?.id;
	return draftOwner({
		authDisabled: snapshot.kind === 'local',
		publicViewer: snapshot.kind === 'publicViewer',
		localDesktop: snapshot.kind === 'localDesktop',
		accountId: typeof id === 'string' || typeof id === 'number' ? String(id) || null : null
	});
}

/** 1 回の遷移の判断。 */
export interface DraftSessionStep {
	next: DraftSessionPhase;
	/** `chronogazer.freeTrends.draft:` で始まるキーを全部捨てる。 */
	discardAll: boolean;
	/** 印の書き換え。`undefined` = そのまま、`null` = 消す、文字列 = その持ち主にする。 */
	mark: string | null | undefined;
	/** セッションの世代を進める。 */
	advance: boolean;
}

function sessionKey(phase: DraftSessionPhase): string {
	switch (phase.kind) {
		case 'initial':
		case 'loading':
			return 'pre';
		case 'ended':
			return 'ended';
		case 'active':
			return `active:${phase.owner ?? ''}`;
	}
}

/**
 * 見張りが snapshot を見るたびの判断（純関数）。`mark` は sessionStorage の印（読めなければ `null`）。
 */
export function draftSessionStep(
	prev: DraftSessionPhase,
	snapshot: DraftSessionSnapshot,
	mark: string | null
): DraftSessionStep {
	let next: DraftSessionPhase;
	let discardAll: boolean;
	let markOut: string | null | undefined;
	if (snapshot.status === 'unknown') {
		if (prev.kind === 'initial' || prev.kind === 'loading') {
			// 再読み込み直後の取り戻し。捨てない（印も残す）。
			return { next: { kind: 'loading' }, discardAll: false, mark: undefined, advance: false };
		}
		// `active` の後の `unknown` = 資格情報が替わった（hold）。`ended` の後なら既に捨ててある。
		next = { kind: 'ended' };
		discardAll = true;
		markOut = null;
	} else if (snapshot.status === 'none') {
		next = { kind: 'ended' };
		discardAll = true;
		markOut = null;
	} else {
		const owner = draftOwnerOfSnapshot(snapshot);
		next = { kind: 'active', owner };
		if (owner === null) {
			// 閲覧公開・ログイン不要モードなど、下書きを持たないセッション。
			discardAll = true;
			markOut = null;
		} else if (prev.kind === 'active' && prev.owner === owner) {
			discardAll = false;
			markOut = mark === owner ? undefined : owner;
		} else if (prev.kind !== 'active' && prev.kind !== 'ended' && mark === owner) {
			// 再読み込みで同じ持ち主に戻った。
			discardAll = false;
			markOut = undefined;
		} else {
			discardAll = true;
			markOut = owner;
		}
	}
	return {
		next,
		discardAll,
		mark: markOut,
		advance: sessionKey(prev) !== sessionKey(next)
	};
}

/** 印を読む（ベストエフォート）。 */
export function readDraftOwnerMark(storage: DraftStorage | undefined): string | null {
	if (!storage) return null;
	try {
		return storage.getItem(DRAFT_OWNER_MARK_KEY);
	} catch {
		return null;
	}
}

/** 判断をストレージに当てる（ベストエフォート）。 */
export function applyDraftSessionStep(
	storage: DraftStorage | undefined,
	step: Pick<DraftSessionStep, 'discardAll' | 'mark'>
): void {
	if (!storage) return;
	try {
		if (step.discardAll) {
			const keys: string[] = [];
			for (let i = 0; i < storage.length; i += 1) {
				const key = storage.key(i);
				if (key !== null && key.startsWith(DRAFT_STORAGE_PREFIX)) keys.push(key);
			}
			for (const key of keys) storage.removeItem(key);
		}
		if (step.mark === null) storage.removeItem(DRAFT_OWNER_MARK_KEY);
		else if (step.mark !== undefined) storage.setItem(DRAFT_OWNER_MARK_KEY, step.mark);
	} catch {
		// 捨てられなくても表示は続ける（読み書きの側も持ち主で分けている）。
	}
}
