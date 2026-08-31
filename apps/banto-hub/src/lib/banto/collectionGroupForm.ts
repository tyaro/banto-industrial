/**
 * T18-6b（TAG-UX-7/TAG-UX-8、2026-08-27 オーナー決定「収集グループの作成／
 * 再設定を Drawer に寄せる」）: `CollectionGroupDrawer.svelte`（
 * `collection-groups/+page.svelte` と将来のタグツリー右クリック（T18-6d）
 * 双方から使う共通部品）が必要とする、依存ゼロの純関数・定数・フォーム状態の
 * 型だけを切り出す。T18-6a の `plcConnectionForm.ts` と同じ方針 — Svelte 側は
 * `$state`/DOM 組み立てに専念させ、ここに置く関数はスナップショット値だけを
 * 引数に取り、テストしやすく保つ。
 *
 * 本モジュールが担う2つの役割:
 *
 * 1. **フォーム状態の組み立て**（旧 `collection-groups/+page.svelte` が
 *    ページ内に持っていた `FormState`/`blankForm`/`formFromGroup`/`toInput`
 *    を無改変で移設 — 検証・既定値を一切変えていない）。
 * 2. **新規作成時の連番名プリフィル**（TAG-UX-8「空欄で出さず `group1` の
 *    ように、既存のグループ名と衝突しない最小の連番を初期値に入れる」）:
 *    {@link nextGroupName}。採番ロジック自体は `plcConnectionForm.ts` の
 *    `nextConnectionName` と共通の {@link nextSequentialName}
 *    （`sequentialName.ts`）を使う。
 *
 * 修正1（実機で再現した不具合、2026-08-31 オーナー報告）: {@link nextGroupName}
 * は既存レコード名に加えて `pendingNames`（pending queue 内の未適用の作成分
 * の名前 - `pendingCreateNames.ts::pendingCreateNames` で抽出したもの）も
 * 衝突候補として受け取れる。収集稼働中の作成は 202 でキューに入るだけで
 * DB（＝ `existingNames`）には現れないため、`pendingNames` を見ないと
 * 「稼働中に同じ Drawer を複数回開くと毎回同じ名前が提案され、後から一括
 * 適用すると名前の一意制約で全滅する」（オーナーが実機で再現: 収集稼働中に
 * 3回作成 → 3回とも `group1` が提案され全部衝突）。
 */
import { nextSequentialName } from './sequentialName';
import type { CollectionGroup, CollectionGroupInput } from './tagRegistryAdmin';

/**
 * TAG-UX-8: 新規作成フォームの名前プリフィル。`prefix`（既定 `"group"`）に
 * 続く数字部分だけを見て、`existingNames` と `pendingNames` の両方に
 * 含まれない最小の正整数を選ぶ（挙動は `plcConnectionForm.ts::
 * nextConnectionName` と同一 - {@link nextSequentialName} を参照）。
 *
 * `pendingNames` は既定 `[]`（省略可）- 呼び出し側が pending queue の
 * 取得に失敗した場合や、まだ取得前の初期表示ではこの引数を省略して
 * `existingNames` だけで採番してよい（モジュール doc comment の修正1参照）。
 */
export function nextGroupName(
	existingNames: readonly string[],
	prefix = 'group',
	pendingNames: readonly string[] = []
): string {
	return nextSequentialName([...existingNames, ...pendingNames], prefix);
}

/** 編集フォーム状態（作成/編集共通）。数値入力は文字列で保持し、空欄=未設定。 */
export interface CollectionGroupFormState {
	name: string;
	plcConnectionId: string;
	periodMs: string;
	enabled: boolean;
}

/**
 * 新規作成フォームの初期値。`name`/`plcConnectionId` はここでは空
 * （`plcConnectionId` は接続の既定を持たない）のまま返す — TAG-UX-8 の連番
 * プリフィルと T18-6d 向けの接続プリセットは、呼び出し側
 * （`CollectionGroupDrawer.svelte`）が `blankGroupForm()` の直後に代入する
 * （`plcConnectionForm.ts::blankConnectionForm` と同じ役割分担）。
 *
 * `defaultPeriodMs` は呼び出し側が渡す（`ALLOWED_PERIOD_MS[0]` = 100ms、
 * 旧ページ実装をそのまま踏襲）。本モジュールは「依存ゼロの純関数」方針
 * （冒頭コメント）を保つため `tagRegistryAdmin.ts` から値としての
 * `ALLOWED_PERIOD_MS` を import しない — `plcConnectionForm.ts` が
 * `DEFAULT_PORTS` をローカルに持つのと同じ理由に加えて、値 import は
 * `@banto/admin-core`（Svelte 5 rune を使う `.svelte.ts`）を推移的に
 * 引き込み、この最小 vitest 構成では `ReferenceError: $state is not
 * defined` になる（`tagRegistryAdmin.test.ts` の doc comment 参照）。
 * `type` import のみなら完全に消去されるため問題ない。
 */
export function blankGroupForm(defaultPeriodMs: number): CollectionGroupFormState {
	return {
		name: '',
		plcConnectionId: '',
		periodMs: String(defaultPeriodMs),
		enabled: true
	};
}

/** 保存済みグループをフォーム状態へ変換する（編集フォームの初期値）。 */
export function groupToForm(g: CollectionGroup): CollectionGroupFormState {
	return {
		name: g.name,
		plcConnectionId: String(g.plcConnectionId),
		periodMs: String(g.periodMs),
		enabled: g.enabled
	};
}

/** フォーム状態を API 入力（`CollectionGroupInput`）へ変換する。 */
export function formToGroupInput(form: CollectionGroupFormState): CollectionGroupInput {
	return {
		name: form.name,
		plcConnectionId: Number(form.plcConnectionId),
		periodMs: Number(form.periodMs),
		enabled: form.enabled
	};
}
