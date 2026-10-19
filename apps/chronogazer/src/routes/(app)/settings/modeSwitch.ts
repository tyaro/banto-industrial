/**
 * 「運用モード」カテゴリの文言と操作（#623 PR-4。設計は
 * docs/chronogazer-service-design.md §5 の「設定モード ⇔ 運用モード」と、その PR-4 の
 * 実装メモ）。
 *
 * この画面が出るのは Tauri のデスクトップの窓（= この PC の中で記録している設定モード）で、
 * admin のときだけ（`+layout.ts` の `visible.mode`）。サービスの画面（運用モード）からの
 * 「設定モードへ」はトレイの「設定モードに切り替える」が受け持つ（サービスの画面には
 * シェルのコマンドが無い）。
 *
 * 文言の決まり（オーナー決定）: 製品は同梱版で、PC に詳しくない利用者でも迷わないこと。
 * 「サービス」「デスクトップ」の語を前に出さず、「このPCで常時記録する（運用モード）」の
 * ように何が起きるかで書く。切り替えの間は収集が止まることを必ず一言出す。運用モードへの
 * 切り替えでは「Windows の起動時に自動で開始する（推奨）」を既定でチェックする（チェック
 * したままなら管理者の許可（UAC）を 1 回求める）。
 *
 * 判断と文言は純関数にして `modeSwitch.test.ts` で確かめる（画面の部品のテストの仕組みは
 * このアプリに無いので、画面はこの関数を並べるだけにする）。
 */
import { invoke } from '@tauri-apps/api/core';

/** 運用モードの見出し。 */
export const OPERATION_MODE_TITLE = 'このPCで常時記録する（運用モード）';

/** 設定モードの見出し。 */
export const SETTING_MODE_TITLE = 'この画面を開いている間だけ記録する（設定モード）';

/** 切り替えの間は収集が止まる（オーナー決定。必ず出す）。 */
export const PAUSE_NOTE = '切り替えの間は収集が止まります。';

/** 自動開始のチェックの文言（既定でチェック）。 */
export const AUTO_START_LABEL = 'Windows の起動時に自動で開始する（推奨）';

/** 自動開始の既定（オーナー決定 (2): 既定でチェック）。 */
export const AUTO_START_DEFAULT = true;

/** 今のモード（この画面は設定モードの窓でだけ出る）の説明。 */
export const CURRENT_MODE_NOTE =
	'いまは設定モードです。この画面（ChronoGazer）を開いている間だけ記録し、タグや接続の設定を自由に変えられます。';

/** 運用モードの説明。 */
export const OPERATION_MODE_NOTE =
	'運用モードでは、この画面を閉じても・Windows にサインインしていなくても、このPCで記録を続けます。記録のファイルは勝手に変えられないよう保護されます。設定を変えるときは、トレイのアイコンのメニューから「設定モードに切り替える」を選びます。';

/** 自動開始のチェックの状態に合わせた補足（純関数）。 */
export function autoStartNote(autoStart: boolean): string {
	return autoStart
		? 'Windows を起動するたびに自動で記録を始めます。切り替えるときに、管理者の許可（ユーザーアカウント制御）を 1 回求めます。'
		: 'Windows を再起動した後は、自動では記録を始めません。ChronoGazer を開いて「運用を再開」を押してください。今が自動開始の設定なら、切り替えるときに手動に戻します（管理者の許可を 1 回求めます）。';
}

/** 切り替えの前の確認の文（純関数）。 */
export function confirmMessage(autoStart: boolean): string {
	const lines = [
		'運用モード（このPCで常時記録する）に切り替えます。',
		`${PAUSE_NOTE}この画面はいったん閉じ、切り替えが終わると記録の画面が開きます。`
	];
	if (autoStart) {
		lines.push('Windows の起動時の自動開始を設定するため、管理者の許可を求めます。');
	}
	lines.push('よろしいですか?');
	return lines.join('\n');
}

/** 切り替えを受け付けた後に出す文。 */
export const SWITCHING_MESSAGE =
	'運用モードへ切り替えています。この画面はまもなく閉じ、切り替えの進み具合を示す画面が開きます。';

/** 画面の状態（純粋な状態遷移。`ModeSection.svelte` が使う）。 */
export type SwitchUiState =
	| { kind: 'idle' }
	| { kind: 'requesting' }
	| { kind: 'accepted' }
	| { kind: 'failed'; message: string };

/** ボタンを押せるか（受け付けた後は二度と押させない - プロセスが終わるまで待つ）。 */
export function canRequest(state: SwitchUiState): boolean {
	return state.kind === 'idle' || state.kind === 'failed';
}

/**
 * 運用モードへの切り替えを頼む（Tauri のコマンド `switch_to_operation_mode`）。成功は
 * 「受け付けた」だけ（シェルが記録エンジンを止めて終わり、新しいシェルが続きを行う）。
 * 失敗（サービスが入っていない・管理者の許可を断った・別の処理の途中など）は理由の文で
 * reject する。
 */
export async function requestSwitchToOperation(autoStart: boolean): Promise<void> {
	await invoke('switch_to_operation_mode', { autoStart });
}

/** 取り出せない形のエラーに出す一般の文言（`[object Object]` を画面に出さない）。文脈に依らない。 */
export const GENERIC_ERROR_TEXT = '操作に失敗しました。もう一度お試しください。';

/** モード切り替えの画面だけが、取り出せないときに使う文言。 */
export const SWITCH_FAILED_TEXT = '切り替えに失敗しました。もう一度お試しください。';

/**
 * Tauri の invoke が reject する値から、画面に出す文を取り出す（純関数）。Rust の
 * `BantoError` は `{ kind: 'bad_request', message: '…' }` のオブジェクトで届き、検証の
 * エラーは `{ kind: 'validation', field_errors: [{ message }] }`。文字列・`Error` も受ける。
 * どれでも取り出せなければ、呼ぶ側が渡す `fallback`（文脈ごとの文言）を返す。
 */
export function errorText(err: unknown, fallback: string = GENERIC_ERROR_TEXT): string {
	if (typeof err === 'string') return err.trim() === '' ? fallback : err;
	if (err instanceof Error) return err.message.trim() === '' ? fallback : err.message;
	if (typeof err === 'object' && err !== null) {
		const o = err as { message?: unknown; field_errors?: unknown };
		if (typeof o.message === 'string' && o.message.trim() !== '') return o.message;
		if (Array.isArray(o.field_errors)) {
			const texts = o.field_errors
				.map((fe) => (fe as { message?: unknown } | null)?.message)
				.filter((m): m is string => typeof m === 'string' && m.trim() !== '');
			if (texts.length > 0) return texts.join(' / ');
		}
	}
	return fallback;
}
