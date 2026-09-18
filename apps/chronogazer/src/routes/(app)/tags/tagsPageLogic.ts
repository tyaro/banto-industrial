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
 * D. **一覧の取得失敗が「0件」に潰れる**（#394レビュー P1-3）:
 *    `listSectionView`/`createFormGate`（下記「D」）が「未読込 / 読み込み
 *    失敗 / 読み込めて0件」の3状態を分け、失敗した一覧を0行のグリッドとして
 *    描かない・「先に○○を作成してください」を読めて0件のときだけ出す。
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
