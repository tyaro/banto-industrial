/**
 * #342 段階A（docs/tag-server-design.md §4.2「演算タグの式チェック API」）:
 * `POST /api/tags/expression/check` のクライアントと、呼び出しタイミングを
 * 駆動する純粋なロジック（debounce・IME 変換中の抑止・古い応答の破棄）。
 *
 * **なぜロジックを `ExpressionCheckController` に切り出すか**
 * （`deferredDeleteCore.ts` の doc comment「なぜこのファイルは `.svelte.ts`
 * ではないのか」と同じ理由）: このリポジトリの vitest は
 * `@sveltejs/vite-plugin-svelte` を導入しない最小構成で、`$state` を含む
 * `.svelte.ts` を直接 import すると `ReferenceError: $state is not
 * defined` になる。debounce・IME 抑止・レース条件（古いリクエストの応答が
 * 新しいリクエストより後に返ってきても上書きしない）はどれも UI と無関係な
 * 状態機械なので、素の TypeScript のクラスとしてここに置き、Svelte 側の
 * 反応性（`$state` へどう書き込むか）は呼び出し元
 * （`(app)/tags/+page.svelte`）のコールバックに任せる。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

// --- wire types（`apps/banto-hub/core/src/rest.rs` の
// `ExpressionCheckRequest`/`ExpressionCheckResponse` と同じ camelCase 形） ---

/** 参照タグ1件の要約（コンパイル成功時のみ埋まる）。 */
export interface ExpressionRef {
	name: string;
	dataType: string;
	unit: string | null;
	tagKind: string;
}

/** 現在値による試算結果。 */
export interface ExpressionPreview {
	value: number | null;
	evaluated: boolean;
	reason: string | null;
}

/**
 * `banto_expr::CompileError` の全 variant 名を写した `kind` 文字列 - サーバー
 * 側で新しい variant が増えたらここにも追加が要る（型は敢えて `string` に
 * 緩めてあり、未知の `kind` が来てもクラッシュしない - フロントは `kind`を
 * ラベリングにしか使わず、分岐は `pos`/`message` の有無だけで足りるため）。
 */
export type ExpressionCheckErrorKind =
	| 'syntax'
	| 'type_mismatch'
	| 'unknown_function'
	| 'arity_mismatch'
	| 'bad_bit_index'
	| 'bad_bit_target'
	| 'source_too_long'
	| 'too_deep'
	| 'unknown_tag'
	| 'string_ref'
	| 'cycle';

export interface ExpressionCheckError {
	kind: ExpressionCheckErrorKind | string;
	/** バイトオフセット = 文字オフセット（本文法は ASCII のみ）。`source_too_long`だけ`null`。 */
	pos: number | null;
	message: string;
}

/** `POST /api/tags/expression/check` の応答。常に 200 - `ok:false` でも HTTP エラーにならない。 */
export interface ExpressionCheckResult {
	ok: boolean;
	resultType: string | null;
	refs: ExpressionRef[];
	preview: ExpressionPreview;
	error: ExpressionCheckError | null;
}

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

/**
 * `POST /api/tags/expression/check` を叩く。このエンドポイントは式が不正
 * でも 200 + `ok:false` を返す（プレビュー用 API のため - サーバー側の
 * `ExpressionCheckResponse` doc comment参照）ので、ここで投げる
 * {@link ProviderError} は認証切れ・ネットワーク断など「チェックそのものが
 * 実行できなかった」場合に限られる。
 */
export async function checkExpression(
	expression: string,
	externalName: string | null
): Promise<ExpressionCheckResult> {
	const headers: Record<string, string> = { ...CSRF_HEADER, 'Content-Type': 'application/json' };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch('/api/tags/expression/check', {
			method: 'POST',
			headers,
			body: JSON.stringify({ expression, externalName })
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) {
		let body: unknown;
		try {
			body = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(body)) throw new ProviderError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	return (await response.json()) as ExpressionCheckResult;
}

/**
 * #342 段階B: `GET /api/tags/expression/functions` の応答要素。**正は Rust の
 * `banto_expr::BUILTIN_FUNCTIONS`**（型検査の `check_call` が名前と引数個数を
 * 引くのと同じ表）で、フロントは受け取って表示するだけ - 関数表をここに
 * 書き写さない（表が2つに割れてドリフトするのを避けるため、
 * `apps/banto-hub/core/src/rest.rs::tags_expression_functions` の doc comment
 * 参照）。
 */
export interface ExpressionFunction {
	name: string;
	arity: number;
	/** 呼び出し形の見本（例: `if(条件, 真のとき, 偽のとき)`）。補完の `detail` に出す。 */
	signature: string;
	/** 1行の日本語説明。 */
	description: string;
}

/**
 * 組み込み関数表を取得する。内容は**静的**（サーバー側で DB も設定も読まない）
 * なので、呼び出し側は1回取得してキャッシュしてよい。
 *
 * 認可は式チェックと同じ `require_editor` - 失敗（権限不足・ネットワーク断）
 * したら**補完全体を壊さず**タグ候補だけで動かすこと（呼び出し側で握りつぶす
 * 前提なので、ここでは素直に throw する）。
 */
export async function fetchExpressionFunctions(): Promise<ExpressionFunction[]> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch('/api/tags/expression/functions', { headers });
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}
	if (!response.ok) {
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}
	const body = (await response.json()) as { functions?: ExpressionFunction[] };
	return body.functions ?? [];
}

/**
 * `computed` タグの式欄でだけチェックを叩く（他の `tagKind` では
 * `expression` フィールド自体が無意味）。空文字（前後空白のみを含む）は
 * 「未入力」としてチェックを叩かない - {@link ExpressionCheckController}
 * 側でも同じ判定を使う。
 */
export function shouldCheckExpression(tagKind: string, expression: string): boolean {
	return tagKind === 'computed' && expression.trim() !== '';
}

export interface ExpressionCheckControllerOptions {
	/** debounce 待ち時間（既定 300ms - 実装指示どおり）。 */
	delayMs?: number;
	/** 実際にサーバーへ問い合わせる関数（テストではフェイクに差し替える）。 */
	run: (expression: string, externalName: string | null) => Promise<ExpressionCheckResult>;
	/** リクエスト送出の直前に呼ぶ（ローディング表示用、任意）。 */
	onStart?: () => void;
	/**
	 * 結果が確定するたびに呼ぶ - `result` が `null` なのは「空文字になった
	 * ので表示をクリアする」の合図（サーバーには問い合わせていない）。
	 */
	onResult: (result: ExpressionCheckResult | null) => void;
	/** `run` が reject したとき（ネットワーク断・認証切れ等）に呼ぶ。 */
	onError?: (err: unknown) => void;
}

/**
 * 式欄の入力イベントから debounce・IME 変換中の抑止・古い応答の破棄までを
 * まとめて扱う状態機械（このファイルの doc comment参照）。
 *
 * - **debounce**: `scheduleCheck` を連打しても、最後の呼び出しから
 *   `delayMs` 経つまでは実際には叩かない（`Debouncer` 相当をここに統合）。
 * - **IME 抑止**: `onCompositionStart`/`onCompositionEnd` の間は
 *   `scheduleCheck` を呼んでもタイマーを起動しない - 変換確定
 *   （`compositionend`）の瞬間にあらためて1回スケジュールする（実装指示
 *   「IME 変換中は送らない」）。
 * - **古い応答の破棄**: 連続入力で複数のリクエストが飛んだ場合、後から
 *   開始したリクエストより前のリクエストの応答が遅れて返ってきても
 *   `onResult`/`onError` を呼ばない（`#requestSeq` で世代管理）。
 */
export class ExpressionCheckController {
	#delayMs: number;
	#run: ExpressionCheckControllerOptions['run'];
	#onStart?: () => void;
	#onResult: ExpressionCheckControllerOptions['onResult'];
	#onError?: (err: unknown) => void;
	#timer: ReturnType<typeof setTimeout> | null = null;
	#composing = false;
	#requestSeq = 0;

	constructor(options: ExpressionCheckControllerOptions) {
		this.#delayMs = options.delayMs ?? 300;
		this.#run = options.run;
		this.#onStart = options.onStart;
		this.#onResult = options.onResult;
		this.#onError = options.onError;
	}

	get isComposing(): boolean {
		return this.#composing;
	}

	onCompositionStart = (): void => {
		this.#composing = true;
		this.cancel();
	};

	/** IME 確定時に呼ぶ - 確定直後の内容であらためてスケジュールする。 */
	onCompositionEnd = (expression: string, externalName: string | null): void => {
		this.#composing = false;
		this.scheduleCheck(expression, externalName);
	};

	/**
	 * 式欄の `input` イベントごとに呼ぶ。IME 変換中は何もしない（`compositionend`
	 * で再開される）。空文字なら即座に `onResult(null)` でクリアし、サーバーには
	 * 問い合わせない。
	 */
	scheduleCheck(expression: string, externalName: string | null): void {
		this.cancel();
		if (this.#composing) return;
		if (expression.trim() === '') {
			// このリクエスト世代も無効化する - 直前に飛んでいたリクエストの
			// 応答が後から来ても、空になった後の画面を上書きさせない。
			this.#requestSeq += 1;
			this.#onResult(null);
			return;
		}
		this.#timer = setTimeout(() => {
			this.#timer = null;
			void this.#execute(expression, externalName);
		}, this.#delayMs);
	}

	/** 保留中のタイマーを止める（コンポーネント破棄時・フォームを閉じるとき等）。 */
	cancel(): void {
		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
	}

	async #execute(expression: string, externalName: string | null): Promise<void> {
		const seq = (this.#requestSeq += 1);
		this.#onStart?.();
		try {
			const result = await this.#run(expression, externalName);
			if (seq !== this.#requestSeq) return; // 新しいリクエストに追い越された
			this.#onResult(result);
		} catch (err) {
			if (seq !== this.#requestSeq) return;
			this.#onError?.(err);
		}
	}
}
