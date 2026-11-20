/**
 * 自由トレンドの下書きのセッションの見張り（判断は `freeTrendDraftSession.ts` の純関数）。
 *
 * ルートの `+layout.svelte` から 1 回だけ始める（ログイン画面でも動く）。セッションの制御の
 * `subscribe` は snapshot が変わるたびに同期で呼ばれるので、`active` → `unknown` → `active` のような
 * 一瞬の遷移も取りこぼさない。応答の照合に使う世代も、ここで同期に進む（保存・削除・一覧の応答の
 * 継続より先に進んでいる）。世代は命令的に読むだけなので `$state` にしない（snapshot の
 * 確定の途中で書いても、反応の仕組みに触らない）。
 */
import { getSessionController } from '@banto/admin-core';
import { sessionDraftStorage } from './freeTrendDraft';
import {
	applyDraftSessionStep,
	draftSessionStep,
	readDraftOwnerMark,
	type DraftSessionPhase,
	type DraftSessionSnapshot
} from './freeTrendDraftSession';

let generation = 0;
let phase: DraftSessionPhase = { kind: 'initial' };
let started = false;

/** セッションの世代。有効なセッション（下書きの持ち主）が替わる・無効になるたびに進む。 */
export function draftSessionGeneration(): number {
	return generation;
}

function observe(snapshot: DraftSessionSnapshot): void {
	const storage = sessionDraftStorage();
	const step = draftSessionStep(phase, snapshot, readDraftOwnerMark(storage));
	applyDraftSessionStep(storage, step);
	phase = step.next;
	if (step.advance) generation += 1;
}

/** 見張りを始める（2 回目以降は何もしない）。アプリの寿命の間動かし続ける。 */
export function startDraftSessionWatch(): void {
	if (started) return;
	started = true;
	const controller = getSessionController();
	observe(controller.snapshot);
	controller.subscribe((snapshot) => observe(snapshot));
}
