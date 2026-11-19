/**
 * #1198 PR3: 「表示グループ」ページの「操作の状態」（編集中 → 保存済み・未反映 → 記録中）の
 * 判断（純関数。`applyState.test.ts` が表で固定する）。
 *
 * 材料は 3 つだけ: ペン（`tagId` / `hubTag`）、タグの登録情報（`enabled`）、収集の状態
 * （`GET /api/collect` = `CollectorStateView`）。収集の状態の `running.tagKeys`（PR1）は
 * 「走っている収集が実際に読んでいるタグのキー（`tag:<id>`）」で、開始で採用した設定から
 * 作られる。除外されたタグは `tagKeys` に入らず `exclusions` 側にある。
 *
 * **限界**: `tagKeys` は「採用された」ことだけを示す。同じタグのアドレス・周期・データ型を
 * 「記録」ページで変えた場合の反映は判定しない（それは「記録」ページの既存の案内
 * （`collectStateDetail` の「設定を変えたときは『収集を再起動』」）の範囲）。ここで言う
 * 「未反映」は「タグが収集の対象に入っていない」だけ。
 *
 * **収集の状態が取れていない（`null`）ときは未反映の判定をしない**: 誤って「未反映」と
 * 言わず、状態は「不明」にする。
 */
import type { CollectAction, CollectorStateView } from '../banto/collectAdmin';
import { collectExclusions, qualityLabel } from '../banto/collectAdmin';

export type ApplyKind =
	/** タグ未選択の行（状態は出さない）。 */
	| 'none'
	/** 下書きで足したペン（保存前）。 */
	| 'unsaved'
	/** 保存済み・有効だが、走っている収集の対象に入っていない。 */
	| 'pending'
	/** 開始時に設定不正などで外された。 */
	| 'excluded'
	/** タグが無効。 */
	| 'disabled'
	/** 走っている収集が読んでいる。 */
	| 'recording'
	/** 収集が走っていない（保存済み・有効）。 */
	| 'notRunning'
	/** 収集の状態・タグの登録情報が分からない。 */
	| 'unknown';

export interface ApplyState {
	kind: ApplyKind;
	/** 画面に出す文言（`none` は空）。 */
	label: string;
}

export const APPLY_LABELS = {
	unsaved: '未保存',
	pending: '保存済み・未反映',
	// 既存の文言（collectAdmin.ts `qualityLabel('invalid')`）をそのまま使う。
	excluded: qualityLabel('invalid'),
	disabled: '無効',
	recording: '記録中',
	unknown: '不明'
} as const;

/** 収集が走っていないときの文言（`collectStateLabel` の 4 状態に 1 対 1）。 */
export function notRunningLabel(state: CollectorStateView): string {
	switch (state.state) {
		case 'stopped':
			return '収集停止中';
		case 'starting':
			return '収集開始中';
		case 'noTargets':
			return '収集対象なし';
		case 'startFailed':
			return '収集開始に失敗';
		case 'running':
			return APPLY_LABELS.recording;
	}
}

export interface ApplyPenInput {
	tagId: number | null;
	hubTag: unknown | null;
}

/** 保存前のペンか（`hubTag`、または保存済みの下書きの基準に無いタグ）。 */
export function isPenUnsaved(pen: ApplyPenInput, baselineTagIds: ReadonlySet<number>): boolean {
	if (pen.hubTag !== null) return true;
	return pen.tagId !== null && !baselineTagIds.has(pen.tagId);
}

/**
 * ペン 1 本の反映状態（純関数）。優先順位:
 * 未保存 > タグ未選択 > 登録情報が無い（不明）> 無効 > 収集の状態が不明 > 収集が走っていない >
 * `exclusions`（設定不正）> `tagKeys`（記録中）> それ以外（保存済み・未反映）。
 * `exclusions` を `tagKeys` より先に見るのは、外されたタグを「記録中」と言わないため
 * （タグ自身の `tag:<id>` と、属する収集グループの `grp:<id>` を見る。接続ごと外れた
 * タグも `tag:<id>` の除外として載る）。収集グループ・接続が無効なら、呼び出し側が
 * `tag.enabled` を偽にして渡す（再起動しても対象に入らないので「未反映」と言わない）。
 */
export function penApplyState(
	pen: ApplyPenInput,
	unsaved: boolean,
	tag: { enabled: boolean; collectionGroupId?: number } | undefined,
	collect: CollectorStateView | null
): ApplyState {
	if (unsaved) return { kind: 'unsaved', label: APPLY_LABELS.unsaved };
	if (pen.tagId === null) return { kind: 'none', label: '' };
	if (tag === undefined) return { kind: 'unknown', label: APPLY_LABELS.unknown };
	if (!tag.enabled) return { kind: 'disabled', label: APPLY_LABELS.disabled };
	if (collect === null) return { kind: 'unknown', label: APPLY_LABELS.unknown };
	if (collect.state !== 'running') return { kind: 'notRunning', label: notRunningLabel(collect) };
	const key = `tag:${pen.tagId}`;
	const groupKey = tag.collectionGroupId === undefined ? null : `grp:${tag.collectionGroupId}`;
	if (
		collectExclusions(collect).some(
			(e) => (e.unit === 'tag' && e.key === key) || (e.unit === 'group' && e.key === groupKey)
		)
	) {
		return { kind: 'excluded', label: APPLY_LABELS.excluded };
	}
	if (collect.tagKeys.includes(key)) return { kind: 'recording', label: APPLY_LABELS.recording };
	return { kind: 'pending', label: APPLY_LABELS.pending };
}

// --- 中央の上の帯 ---------------------------------------------------------

export interface ApplyBandInput {
	/** 下書きが未保存か。 */
	dirty: boolean;
	collect: CollectorStateView | null;
	states: readonly ApplyState[];
	canWrite: boolean;
	/** 「記録へ反映」の実行中（二重押しを防ぐ）。 */
	busy: boolean;
}

export type ApplyBandKind = 'editing' | 'pending' | 'recording' | 'notRunning' | 'unknown';

export interface ReflectButton {
	action: Extract<CollectAction, 'start' | 'restart'>;
	label: string;
	disabled: boolean;
	/** 無効の理由（`null` = 有効）。 */
	title: string | null;
	/** `window.confirm` に出す、影響の説明。 */
	confirm: string;
}

export interface ApplyBand {
	kind: ApplyBandKind;
	text: string;
	/** 反映が要る（保存済み・有効で記録に入っていない）ペンの数。 */
	reflectCount: number;
	/** `null` = ボタンを出さない。 */
	reflect: ReflectButton | null;
}

export const REFLECT_LABEL = '記録へ反映';
export const REFLECT_DIRTY_TITLE = '未保存の変更を保存してから反映してください';
export const REFLECT_BUSY_TITLE = '実行中です';
export const REFLECT_RESTART_CONFIRM =
	'収集を再起動して、保存したタグを記録に反映します。再起動の間（数秒）はすべてのタグの記録が途切れます。よろしいですか？';
export const REFLECT_START_CONFIRM =
	'収集を開始して、保存したタグを記録に反映します。よろしいですか？';

/**
 * 帯の文言とボタン（純関数）。一行: 「編集中（未保存）」→「保存済み・未反映 n 件」→「記録中」。
 * 収集が走っていないときは「収集停止中…」等で、保存済み・有効なペンがあれば「開始」で反映する。
 * 収集の状態が分からないときは「不明」とだけ言い、ボタンは出さない。
 */
export function applyBand(input: ApplyBandInput): ApplyBand {
	const { dirty, collect, states, canWrite, busy } = input;
	const count = states.filter((s) => s.kind === 'pending' || s.kind === 'notRunning').length;
	const noButton = (kind: ApplyBandKind, text: string): ApplyBand => ({
		kind,
		text,
		reflectCount: count,
		reflect: null
	});
	const button = (action: 'start' | 'restart'): ReflectButton | null => {
		if (!canWrite || count === 0) return null;
		return {
			action,
			label: REFLECT_LABEL,
			disabled: dirty || busy,
			title: dirty ? REFLECT_DIRTY_TITLE : busy ? REFLECT_BUSY_TITLE : null,
			confirm: action === 'restart' ? REFLECT_RESTART_CONFIRM : REFLECT_START_CONFIRM
		};
	};
	const withButton = (
		kind: ApplyBandKind,
		text: string,
		action: 'start' | 'restart'
	): ApplyBand => ({ kind, text, reflectCount: count, reflect: button(action) });

	if (collect === null) {
		return noButton(dirty ? 'editing' : 'unknown', dirty ? '編集中（未保存）' : '収集の状態: 不明');
	}
	const running = collect.state === 'running';
	const startable =
		collect.state === 'stopped' || collect.state === 'noTargets' || collect.state === 'startFailed';
	const action = running ? 'restart' : 'start';
	if (dirty) {
		return {
			kind: 'editing',
			text: '編集中（未保存）',
			reflectCount: count,
			reflect: running || startable ? button(action) : null
		};
	}
	if (running) {
		return count > 0
			? withButton('pending', `${APPLY_LABELS.pending} ${count} 件`, 'restart')
			: noButton('recording', APPLY_LABELS.recording);
	}
	const text = notRunningLabel(collect);
	if (!startable) return noButton('notRunning', text);
	return count > 0
		? withButton('notRunning', `${text}（有効なタグ ${count} 件は開始すると記録されます）`, 'start')
		: noButton('notRunning', text);
}
