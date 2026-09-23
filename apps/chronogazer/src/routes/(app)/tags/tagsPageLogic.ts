/**
 * `tags/+page.svelte` の判断を純関数として切り出したもの（#391 レビュー
 * 対応、Refs #383/#391）。`+page.svelte` は Svelte 5 rune（`$state`）を使う
 * ため、このリポジトリの最小 vitest 構成（`@sveltejs/vite-plugin-svelte`
 * 無し）では直接ロードできない - `settings/categories.ts`（同じ chronogazer
 * の別ルート）に倣い、状態を持たない判断だけをここに出して依存ゼロで
 * テストする。
 *
 * 収める理由は4件のレビュー指摘に対応する:
 *
 * A. **保存中の行切替による取り違え**: PLC接続・収集グループ・タグの
 *    3セクションはどれも「行を選択→編集フォームを表示→保存」という同型の
 *    流れを持つ。保存中に別の行へ選択が移ると（`selectTag()`が
 *    `editTagStore`を新しいインスタンスに差し替える）、後から返ってくる
 *    更新応答が「今開いているフォーム」に誤って適用され、タグAのIDに
 *    タグBの内容を送って上書きできてしまう。`runGuardedSave`は「保存を
 *    開始した時点の対象ID・フォームストアの参照」（`SaveGuardToken`）を
 *    控えておき、応答が返ってきた時点の「今の」対象と突き合わせ
 *    （`isSaveStillCurrent`）、一致する場合だけ適用可能な結果として返す。
 *    一致しなければ `stale-success`/`stale-error` を返し、呼び出し側は
 *    画面更新もトーストも行わない（ユーザーは既に別の行を見ているため）。
 *
 *    `+page.svelte`側で「保存中は選択・削除を禁止する」ガードも入れるが、
 *    それだけでは不十分（将来ほかの経路 - 一覧の再読み込み、コマンド
 *    パレット等 - が選択を変えても壊れないようにするため、応答側の照合を
 *    不変条件として持つ）。
 *
 *    #394追補: 削除にも同じ欠陥類型があった - 削除の`await`中に別の行を
 *    選び直すと、応答が返った時点で`selectedX = null`が無条件に走り、
 *    その行で編集中だった未保存の入力が黙って消える（#378の「未保存の
 *    入力を黙って捨てない」方針に反する）。`runGuardedDelete`/
 *    `isDeleteStillCurrent`（下記「A': 削除中の行切替による取り違え」）が
 *    同じ形の対策をIDだけの照合で提供する。
 *
 * B. **スケーリングの検証エラーの無表示**: バックエンドの
 *    `Scaling::from_parts()`（`crates/banto-tags/src/scaling.rs`）は
 *    「4項目のうち一部だけ指定」で `field: "scaling"` を返すが、フォームに
 *    あるのは `RawLo`/`RawHi`/`EngLo`/`EngHi` の4項目で `Scaling` という
 *    フィールドは無い。`splitServerFieldErrors`はフォームのスキーマに
 *    実在するフィールドだけを`setServerErrors`用に整形し、`scaling`は
 *    4項目全部への同一メッセージとして明示的に展開する。対応先が
 *    まったく無い（スキーマにも scaling の展開先にも無い）エラーは必ず
 *    トースト行きに回し、「エラーが無表示」を構造的に起こさない。
 *
 * C. **削除拒否理由の消失**: `ProviderError.message`は検証エラーでは
 *    一律`"validation failed"`（`@banto/admin-core`の`errors.ts`の
 *    `describe()`）になり、具体的な理由が消える。`joinFieldErrorMessages`は
 *    `field_errors`の`message`を連結し、「この接続を使用している収集
 *    グループがN件あるため削除できません」のような理由をそのまま
 *    トーストに出せるようにする。
 *
 * A''. **一覧の再取得が順不同で解決する**（観点別レビュー P2-C）:
 *    `runGuardedListLoad`/`isListLoadCurrent`（下記「A''」）が一覧ごとの
 *    世代番号を照合し、後から着いた古いスナップショットで新しい一覧を
 *    上書きしないようにする。
 *
 * D. **一覧の取得失敗が「0件」に潰れる**（#394レビュー P1-3）:
 *    `listSectionView`/`createFormGate`（下記「D」）が「未読込 / 読み込み
 *    失敗 / 読み込めて0件」の3状態を分け、失敗した一覧を0行のグリッドとして
 *    描かない・「先に○○を作成してください」を読めて0件のときだけ出す。
 *
 * E. **接続単位シミュレーション**（#413）: 「値は記録されません」の文言、
 *    切替を保存したときの「収集を再起動」の案内、シミュレーションで値が
 *    動かないタグの見せ方（判定そのものは Rust 側。下記「E」）。
 */

export interface FieldError {
	field: string;
	message: string;
}

// --- A: 保存中の行切替による取り違え ---------------------------------------

/**
 * 保存開始時に控える「対象」。`store`はフォームストアの参照そのもの
 * （行を選び直すたびに`editTagStore`等は新しいインスタンスに差し替わる
 * ので、参照が変われば「別の行を選び直した」と判定できる）。ID だけでなく
 * ストアも比較するのは、将来IDの再利用や別経路でのストア差し替えがあっても
 * 「同じ編集対象・同じフォーム」であることを厳密に保証するため。
 */
export interface SaveGuardToken<TStore> {
	id: number;
	store: TStore;
}

/** 応答が返ってきた時点で「まだ保存開始時と同じ対象を編集中か」を判定する。ID・ストア参照の両方が一致したときだけ true。 */
export function isSaveStillCurrent<TStore>(
	pending: SaveGuardToken<TStore>,
	current: { id: number | null | undefined; store: TStore }
): boolean {
	return current.id === pending.id && current.store === pending.store;
}

export type GuardedSaveOutcome<TEntity> =
	| { kind: 'applied'; entity: TEntity }
	| { kind: 'stale-success' }
	| { kind: 'error'; err: unknown }
	| { kind: 'stale-error' };

/**
 * 保存リクエスト（`request`）を送ってから、応答が返ってきた時点で
 * `isSaveStillCurrent`により「まだ同じ対象を編集中か」を確認したうえで
 * 結果を分類する。`readCurrent`は呼び出し時点ではなく応答が返った時点の
 * 状態を読む必要があるため遅延評価の関数として渡す（`selectedTag`等の
 * `$state`は呼び出し側のクロージャで読む - このモジュール自体は Svelte に
 * 依存しない）。
 *
 * 一致しない場合の2通り:
 * - `stale-success`: 更新自体はサーバーで成立した（一覧の再読み込みは
 *   行ってよい）が、今開いているフォームには適用しない。
 * - `stale-error`: 失敗はもう見えている行と無関係なので、エラー表示も
 *   トーストも出さない（ユーザーは既に別の行を見ているため、古いエラーを
 *   見せても意味がない）。
 */
export async function runGuardedSave<TEntity, TStore>(
	pending: SaveGuardToken<TStore>,
	request: Promise<TEntity>,
	readCurrent: () => { id: number | null | undefined; store: TStore }
): Promise<GuardedSaveOutcome<TEntity>> {
	try {
		const entity = await request;
		return isSaveStillCurrent(pending, readCurrent())
			? { kind: 'applied', entity }
			: { kind: 'stale-success' };
	} catch (err) {
		return isSaveStillCurrent(pending, readCurrent())
			? { kind: 'error', err }
			: { kind: 'stale-error' };
	}
}

// --- A': 削除中の行切替による取り違え（#394 追補） --------------------------
//
// 保存側と同じ欠陥類型: 削除の`await`中に別の行を選び直すと、応答が返った
// 時点で`selectedX = null`が無条件に走り、その行で編集中だった未保存の
// 入力が黙って消える（#378で決めた「未保存の入力を黙って捨てない」方針との
// 不整合）。削除はフォームストアを持たない（送信するのは対象IDだけで、
// フォームの中身は関係ない）ため、`SaveGuardToken`のようにストア参照まで
// 控える必要が無く、IDだけの照合で「まだ同じ行を選んでいるか」を判定できる。

/** 削除開始時に控える対象ID。フォームストアが無いので保存側の`SaveGuardToken`より単純（ID のみ）。 */
export interface DeleteGuardToken {
	id: number;
}

/** 応答が返ってきた時点で「まだ削除開始時と同じ行を選んでいるか」を判定する。 */
export function isDeleteStillCurrent(
	pending: DeleteGuardToken,
	currentId: number | null | undefined
): boolean {
	return currentId === pending.id;
}

export type GuardedDeleteOutcome =
	| { kind: 'applied' }
	| { kind: 'stale-success' }
	| { kind: 'error'; err: unknown }
	| { kind: 'stale-error' };

/**
 * 削除リクエスト（`request`）を送ってから、応答が返ってきた時点で
 * `isDeleteStillCurrent`により「まだ同じ行を選んでいるか」を確認したうえで
 * 結果を分類する（`runGuardedSave`のID版）。`readCurrentId`は応答が返った
 * 時点の状態を読む必要があるため遅延評価の関数として渡す。
 *
 * - `stale-success`: 削除自体はサーバーで成立した（一覧の再読み込みは
 *   行ってよい）が、ユーザーは既に別の行を選んでいるので、その選択を
 *   `null`にしない（＝別の行の未保存入力を消さない）。
 * - `stale-error`: 失敗はもう見えている行と無関係なので、トーストは
 *   出さない（保存側の`stale-error`と同じ考え方）。
 */
export async function runGuardedDelete(
	pending: DeleteGuardToken,
	request: Promise<void>,
	readCurrentId: () => number | null | undefined
): Promise<GuardedDeleteOutcome> {
	try {
		await request;
		return isDeleteStillCurrent(pending, readCurrentId())
			? { kind: 'applied' }
			: { kind: 'stale-success' };
	} catch (err) {
		return isDeleteStillCurrent(pending, readCurrentId())
			? { kind: 'error', err }
			: { kind: 'stale-error' };
	}
}

// --- A'': 一覧の再取得が順不同で解決する（レビュー P2-C） --------------------
//
// `reloadConnections()`/`reloadGroups()`/`reloadTags()` は `await` のあとで
// 無条件に一覧へ書き戻していた。作成と保存を続けて行うと `GET` が 2 本飛び、
// **着順は発行順と一致しない**ので、後から着いた古いスナップショットが
// 新しい一覧を上書きする（ユーザーは「更新しました」の直後に更新前の一覧を
// 見る）。#387 で潰した「飛行中の応答が新しい状態を巻き戻す」と同型で、
// `HubSection.svelte` 側の `isPollGenerationCurrent` に対応するものが
// こちらには無かった。
//
// 直し方は同じ: **一覧ごとに世代番号**を持ち、再取得の開始時に控えて、
// 応答を適用するときに現在の世代と照合する。

/** 再取得の応答を今も適用してよいか（世代照合、純関数）。`isPollGenerationCurrent`（`hubAdmin.ts`）のリスト版。 */
export function isListLoadCurrent(startedAtGeneration: number, current: number): boolean {
	return startedAtGeneration === current;
}

export type GuardedListLoadOutcome<T> =
	{ kind: 'applied'; items: T[] } | { kind: 'stale' } | { kind: 'error'; err: unknown };

/**
 * 一覧の再取得（`request`）を送ってから、応答が返ってきた時点で
 * `isListLoadCurrent` により「これがまだ最新の再取得か」を確認したうえで
 * 結果を分類する（`runGuardedSave`/`runGuardedDelete` と同じ書き方。
 * `readCurrentGeneration` は応答が返った時点の世代を読む必要があるため
 * 遅延評価の関数として渡す）。
 *
 * `stale` は**成功・失敗のどちらからも返る**: より新しい再取得が走って
 * いる以上、古い応答は一覧にもエラー表示にも出さない（新しい方の結果が
 * そのどちらも決める）。
 */
export async function runGuardedListLoad<T>(
	startedAtGeneration: number,
	request: Promise<T[]>,
	readCurrentGeneration: () => number
): Promise<GuardedListLoadOutcome<T>> {
	try {
		const items = await request;
		return isListLoadCurrent(startedAtGeneration, readCurrentGeneration())
			? { kind: 'applied', items }
			: { kind: 'stale' };
	} catch (err) {
		return isListLoadCurrent(startedAtGeneration, readCurrentGeneration())
			? { kind: 'error', err }
			: { kind: 'stale' };
	}
}

// --- B: サーバー検証エラーのフォーム/トースト振り分け -----------------------

function capitalize(s: string): string {
	return s.length === 0 ? s : s[0].toUpperCase() + s.slice(1);
}

/**
 * `${prefix}${Capitalized}`形式のフォームフィールド名の元になった、
 * サーバーの wire フィールド名を復元する（例: `wireFieldName("plcCreate",
 * "plcCreateName")` → `"name"`）。
 */
export function wireFieldName(prefix: string, formFieldName: string): string {
	const suffix = formFieldName.startsWith(prefix)
		? formFieldName.slice(prefix.length)
		: formFieldName;
	return suffix.length === 0 ? suffix : suffix[0].toLowerCase() + suffix.slice(1);
}

/** フォームスキーマが実際に持っている wire フィールド名の一覧（`splitServerFieldErrors`がフォームに出せる項目かどうかの判定に使う）。 */
export function schemaWireFields(prefix: string, fields: readonly { name: string }[]): string[] {
	return fields.map((f) => wireFieldName(prefix, f.name));
}

/** バックエンドの`Scaling::from_parts()`が返す`field: "scaling"`の展開先。フォームには`Scaling`という1項目は無く、4つの生値/工学値項目に分かれているため。 */
const SCALING_TARGET_WIRE_FIELDS = ['rawLo', 'rawHi', 'engLo', 'engHi'] as const;

export interface SplitFieldErrorsResult {
	/** `store.setServerErrors()`にそのまま渡せる形（`field`は`${prefix}${Capitalized}`済み）。 */
	formErrors: FieldError[];
	/** フォームに出す先が無かったメッセージ（トーストへフォールバック）。 */
	toastMessages: string[];
}

/**
 * サーバーの`field_errors`を、フォームに出せる分とトーストに出す分へ
 * 振り分ける。`scaling`は明示的に4項目へ展開し（B の直し方 1）、それ以外で
 * スキーマに無いフィールドは必ずトーストへ回す（B の直し方 2） -
 * 「対応するフォーム項目が無い検証エラーが画面にもトーストにも出ない」を
 * 構造的に起こさない。
 */
export function splitServerFieldErrors(
	prefix: string,
	knownWireFields: readonly string[],
	fieldErrors: readonly FieldError[]
): SplitFieldErrorsResult {
	const formErrors: FieldError[] = [];
	const toastMessages: string[] = [];
	for (const fe of fieldErrors) {
		if (fe.field === 'scaling') {
			const targets = SCALING_TARGET_WIRE_FIELDS.filter((t) => knownWireFields.includes(t));
			if (targets.length === 0) {
				toastMessages.push(fe.message);
				continue;
			}
			for (const target of targets) {
				formErrors.push({ field: `${prefix}${capitalize(target)}`, message: fe.message });
			}
			continue;
		}
		if (knownWireFields.includes(fe.field)) {
			formErrors.push({ field: `${prefix}${capitalize(fe.field)}`, message: fe.message });
		} else {
			toastMessages.push(fe.message);
		}
	}
	return { formErrors, toastMessages };
}

// --- C: 削除拒否理由などの検証エラーメッセージ -------------------------------

/**
 * `ProviderError.message`は検証エラーでは一律`"validation failed"`
 * （`@banto/admin-core`の`errors.ts`の`describe()`）になり、具体的な理由が
 * 消える。`field_errors`の`message`を連結して人間可読な理由に戻す
 * （複数あれば読みやすく`" / "`で区切る）。
 */
export function joinFieldErrorMessages(fieldErrors: readonly FieldError[]): string {
	return fieldErrors.map((fe) => fe.message).join(' / ');
}

// --- D: 一覧の「未読込 / 読み込み失敗 / 読み込めて0件」の区別 ---------------
//
// #394レビュー P1-3: 3つの一覧（PLC接続 / 収集グループ / タグ）は失敗時に
// トーストを出すだけで、変数は初期値`[]`のままだった。トーストが消えると
// グリッドは0行のまま残り、「1件も登録されていない」という**永続的な誤表示**
// になる。さらに作成フォームの分岐が「先にPLC接続を1件以上作成してください」
// という**誤った指示**まで出し、利用者を重複登録へ誘導していた。
//
// 同じアプリのHub側は既にこの規律を持っている（`hubAdmin.ts`の
// `HubView.tags: HubTag[] | null`で「この往復では読めていない」と「読めた
// 結果0件」を別物にし、`HubSection.svelte`が表示に反映する）。ここでは
// その書き方をタグ設定画面に持ち込み、判断だけを純関数に出して総当たりで
// テストする（チェックリスト§5「エラーを『空』に潰さない」「判断は純関数に
// 出して状態の総当たりを表でテストする」）。

/**
 * 1つの一覧の読み込み状態。`items === null`は「まだ読めていない（未読込 or
 * 失敗）」で、`[]`（読めた結果0件）とは**別物**。`error`は直近の読み込みが
 * 失敗した理由（成功したら`null`に戻す）。
 *
 * 読み込みに失敗しても、既に読めていた`items`は捨てない - 最後に読めた
 * 内容を残したまま「更新できなかった」と添える方が、0行に戻すより正確。
 */
export interface ListLoadState<T> {
	items: T[] | null;
	error: string | null;
}

/**
 * 一覧セクションの見せ方:
 * - `loading`: まだ読めていない（失敗もしていない）。読み込み中の表示。
 * - `failed`: 一度も読めておらず、失敗した。**グリッドを0件として描かず**、
 *   失敗した旨と再試行の導線を出す。
 * - `grid`: 読めている（0件でもこちら）。グリッドを描く。
 */
export type ListSectionView = 'loading' | 'failed' | 'grid';

export function listSectionView<T>(state: ListLoadState<T>): ListSectionView {
	if (state.items !== null) return 'grid';
	return state.error !== null ? 'failed' : 'loading';
}

/** 再試行の導線を出すか。直近の読み込みが失敗しているとき（未読込の失敗でも、読めた内容が残ったままの更新失敗でも）。 */
export function showsRetry<T>(state: ListLoadState<T>): boolean {
	return state.error !== null;
}

/** グリッドに渡す行。`null`（未読込）でも`[]`を渡すが、**その場合グリッド自体を描かない**（`listSectionView`が`grid`を返さない）のが前提。 */
export function listRows<T>(state: ListLoadState<T>): T[] {
	return state.items ?? [];
}

/**
 * 依存する一覧（収集グループにとってのPLC接続、タグにとっての収集グループ）の
 * 状態から決まる、作成フォームの出し方:
 * - `form`: 依存先が読めていて1件以上ある。フォームを出す。
 * - `needs-prerequisite`: 依存先が**読めた結果0件**。「先に○○を作成して
 *   ください」を出してよいのはこのときだけ。
 * - `dependency-loading` / `dependency-failed`: 依存先を読めていない。
 *   `<select>`の候補が空のフォームを出すと「候補が無い」ように見えるので
 *   出さず、読み込み中である／読めなかったことをそのまま伝える。
 */
export type CreateFormGate =
	'form' | 'needs-prerequisite' | 'dependency-loading' | 'dependency-failed';

export function createFormGate<T>(dependency: ListLoadState<T>): CreateFormGate {
	switch (listSectionView(dependency)) {
		case 'grid':
			return (dependency.items as T[]).length === 0 ? 'needs-prerequisite' : 'form';
		case 'failed':
			return 'dependency-failed';
		default:
			return 'dependency-loading';
	}
}

// --- E: 接続単位シミュレーション（#413、2026-09-23 オーナー決定） ------------
//
// R1-B では「banto-hub 固有」として閉じていた接続単位シミュレーションを
// 開けた（「実機が無いときに設定できないのは使い物にならない」）。
// シミュレーション接続の値は現在値・イベントには出るが**データファイルには
// 記録されない**（`banto-collect` の約束。
// `apps/chronogazer/core/tests/simulation_not_recorded.rs` が固定）ので、
// 画面はそれを常に分かるようにする。ここに置くのは文言と見せ方の判断だけで、
// **「どの番地なら値が動くか」の判定は持たない**（Rust の
// `banto_collect::simulation::classify_plc_tag` が唯一の判定元。画面は
// `listSimulationCoverage` の結果を表示するだけ）。

/** 接続一覧の「動作」列に出す文言。 */
export const SIMULATION_CONNECTION_LABEL = 'シミュレーション（値は記録されません）';
export const REAL_CONNECTION_LABEL = '実機';

/** 接続一覧の「動作」列（純関数）。レジストリの値を表す（走っている収集の姿は `/settings/collect`）。 */
export function connectionModeLabel(simulation: boolean): string {
	return simulation ? SIMULATION_CONNECTION_LABEL : REAL_CONNECTION_LABEL;
}

/**
 * PLC接続の作成・更新に成功したときのトースト文言（純関数）。
 *
 * **切替は走っている収集に自動では反映されない**（レジストリの変更で自動
 * 再起動しない C-2 の決定）ので、`simulation` が変わった保存には「収集を
 * 再起動」が要ることを必ず添える。保存に失敗したときはこの関数を呼ばない
 * （画面は切り替わったように見せない - 呼び出し側は応答の行を採用する）。
 *
 * `before === null` は新規作成。
 */
export function connectionSavedMessage(
	before: { simulation: boolean } | null,
	after: { simulation: boolean }
): string {
	if (before === null) {
		return after.simulation
			? '作成しました（シミュレーション接続です。値はデータファイルに記録されません）'
			: '作成しました';
	}
	if (before.simulation === after.simulation) return '更新しました';
	return after.simulation
		? '更新しました。シミュレーションに切り替えました（値はデータファイルに記録されません）。収集中の場合は「収集を再起動」で反映されます'
		: '更新しました。実機に切り替えました。収集中の場合は「収集を再起動」で反映されます';
}

/** シミュレーションで値が動かないタグ 1 本（表示用）。 */
export interface UnmovingSimulationTag {
	tagId: number;
	tagName: string;
	address: string;
	connectionName: string;
	reason: string;
}

/**
 * `listSimulationCoverage` の結果から、値が動かない（`supported: false`）
 * タグを表示用に並べる（純関数、並びは結果の順 = タグ id 昇順）。
 *
 * 名前は一覧から引く。一覧の再取得と判定の再取得は別の往復なので、
 * 片方にしか無い行がありうる - そのときは `#id` を出す（行を黙って落とさない）。
 */
export function unmovingSimulationTags(
	entries: readonly {
		tagId: number;
		plcConnectionId: number;
		supported: boolean;
		reason: string | null;
	}[],
	tags: readonly { id: number; name: string; address: string }[],
	connections: readonly { id: number; name: string }[]
): UnmovingSimulationTag[] {
	return entries
		.filter((entry) => !entry.supported)
		.map((entry) => {
			const tag = tags.find((t) => t.id === entry.tagId);
			const conn = connections.find((c) => c.id === entry.plcConnectionId);
			return {
				tagId: entry.tagId,
				tagName: tag?.name ?? `#${entry.tagId}`,
				address: tag?.address ?? '',
				connectionName: conn?.name ?? `#${entry.plcConnectionId}`,
				reason: entry.reason ?? 'シミュレータが値を生成しない番地です'
			};
		});
}

/**
 * 「値が動かないタグ」ブロックの見せ方（純関数）:
 * - `hidden`: シミュレーション接続が 1 本も無い（出す意味が無い）。
 * - `loading` / `failed`: 判定を**まだ読めていない / 読めなかった**。
 *   「全部動く」と言わない（D と同じ規律 - 読めていないを 0 件に潰さない）。
 * - `all-moving`: 読めて、動かないタグが 0 本。
 * - `list`: 読めて、動かないタグがある。
 *
 * **`all-moving` / `list` は「今の設定に対して取得した結果」にだけ出す**
 * （#417 オーナーレビュー P2）。判定は接続の simulation・グループ・タグの
 * どれが変わっても変わるので、再取得を始めるときに前回の結果を捨てる
 * （[`coverageReloadStarted`]）。前回の空配列は「実機接続なので判定対象が
 * 無かった」という意味でしかなく、新しい設定の判定としては使えない。
 */
export type SimulationCoverageView = 'hidden' | 'loading' | 'failed' | 'all-moving' | 'list';

/**
 * 判定の再取得を始めた時点の状態（純関数）。**前回の結果は持ち越さない** -
 * 応答待ちの間は `loading`、失敗したら `failed` になり、古い結果で
 * 「すべて値が動く」と断定しない（[`SimulationCoverageView`] の doc）。
 */
export function coverageReloadStarted<T>(): ListLoadState<T> {
	return { items: null, error: null };
}

/** 判定の再取得が終わった後の状態（純関数）。失敗しても結果を捏造しない（`items` は取得開始時の `null` のまま）。 */
export function coverageReloadSettled<T>(
	started: ListLoadState<T>,
	outcome: { kind: 'applied'; items: T[] } | { kind: 'error'; message: string }
): ListLoadState<T> {
	return outcome.kind === 'applied'
		? { items: outcome.items, error: null }
		: { items: started.items, error: outcome.message };
}

/**
 * 「収集の開始時に外される設定」ブロックの見せ方（#414 段階2、純関数）:
 * - `loading` / `failed`: 判定を**まだ読めていない / 読めなかった**。
 *   「不正な設定はありません」と言わない（読めていないを 0 件に潰さない）。
 * - `none`: 読めて 0 件（ブロックは出さない）。
 * - `list`: 読めて 1 件以上。
 *
 * 判定の再取得は [`coverageReloadStarted`] / [`coverageReloadSettled`] と同じ
 * 形で行う（**前回の結果を持ち越さない** - #417 の P2 と同じ理由。接続・
 * グループ・タグのどれを変えても判定は変わる）。
 */
export type ConfigExclusionsView = 'loading' | 'failed' | 'none' | 'list';

export function configExclusionsView<T>(state: ListLoadState<T>): ConfigExclusionsView {
	switch (listSectionView(state)) {
		case 'loading':
			return 'loading';
		case 'failed':
			return 'failed';
		default:
			return (state.items as T[]).length > 0 ? 'list' : 'none';
	}
}

export function simulationCoverageView<T extends { supported: boolean }>(
	hasSimulationConnection: boolean,
	state: ListLoadState<T>
): SimulationCoverageView {
	if (!hasSimulationConnection) return 'hidden';
	switch (listSectionView(state)) {
		case 'loading':
			return 'loading';
		case 'failed':
			return 'failed';
		default:
			return (state.items as T[]).some((entry) => !entry.supported) ? 'list' : 'all-moving';
	}
}
