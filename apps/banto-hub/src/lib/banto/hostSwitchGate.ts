/**
 * Desktop↔Service 切替ウィザードのゲート判定（純関数）。
 * `/status` の Windows サービスカードと vitest が共有する。
 *
 * preflight: 専用 API が無い間は `last_config_error == null` かつ
 * revision が取得できていることを「実行可能」とみなす（計画どおり）。
 */

/** シェルが報告する現在ビュー。 */
export type HostShellView = 'desktop' | 'service' | 'fallback';

/** ゲート判定に必要な入力。 */
export type HostSwitchGateInput = {
	/** ローカルシェル（Tauri）上か。遠隔ブラウザでは false。 */
	isLocalShell: boolean;
	/** Hub Admin か。 */
	isAdmin: boolean;
	/** Operators または Administrators（シェル側）。 */
	canOperate: boolean;
	/** 現在のシェルビュー。 */
	view: HostShellView;
	/** 切替トランザクション進行中か。 */
	switching: boolean;
	/** `GET /api/status`（`hubStatus.ts`、2026-08-31以前は `/api/v1/status`）の
	 * last_config_error。 */
	lastConfigError: string | null | undefined;
	/** status.revision が取得できているか。 */
	hasRevision: boolean;
};

/**
 * 構成 preflight が通っているか（専用 API 無しの暫定ゲート）。
 */
export function isPreflightOk(
	input: Pick<HostSwitchGateInput, 'lastConfigError' | 'hasRevision'>
): boolean {
	return input.lastConfigError == null && input.hasRevision;
}

/**
 * 「運用モードに切り替える」（サービスへ切り替えて開始）を有効にしてよいか。
 * Desktop または fallback（Offline）から Service へ。
 */
export function canSwitchToService(input: HostSwitchGateInput): boolean {
	if (!input.isLocalShell || !input.isAdmin || !input.canOperate) return false;
	if (input.switching) return false;
	if (input.view === 'service') return false;
	return isPreflightOk(input);
}

/**
 * 「設定モードに戻す」（サービスを停止してこのアプリで Hub を起動）を有効にしてよいか。
 */
export function canSwitchToDesktop(input: HostSwitchGateInput): boolean {
	if (!input.isLocalShell || !input.isAdmin || !input.canOperate) return false;
	if (input.switching) return false;
	return input.view === 'service';
}

/**
 * 自動起動チェックボックスを操作してよいか（現在状態とは独立）。
 */
export function canToggleAutostart(input: HostSwitchGateInput): boolean {
	if (!input.isLocalShell || !input.isAdmin || !input.canOperate) return false;
	return !input.switching;
}

/**
 * カードを無効化するときの利用者向け理由（操作可能なときは null）。
 */
export function hostSwitchDisabledReason(input: HostSwitchGateInput): string | null {
	if (!input.isLocalShell) {
		return 'ローカルシェルが必要です（ブラウザ遠隔からは操作できません）';
	}
	if (!input.isAdmin) {
		return 'Hub 管理者権限が必要です';
	}
	if (!input.canOperate) {
		return 'BantoHub Operators または Windows 管理者権限が必要です';
	}
	if (input.switching) {
		return '切替処理が進行中です';
	}
	if (!isPreflightOk(input)) {
		return input.lastConfigError
			? `構成エラーのため切替できません: ${input.lastConfigError}`
			: '構成ステータスを取得できないため切替できません';
	}
	return null;
}

/** #661 661-5: 切り替えの一言（シェルの窓・確認と同じ文）。 */
export const PAUSE_NOTE = '切り替えの間は収集が止まります。';

/** #661 661-5: Fallback の理由ごとの文言と操作。 */
export type FallbackNotice = {
	title: string;
	message: string;
	/** [設定モードに戻す] を出すか。 */
	offerRestore: boolean;
};

/**
 * シェルが知らせた Fallback の理由（`fallbackCause`）から文言を選ぶ（純関数）。知らない名前・
 * 一般（null）は null（従来の表示だけ）。シェルの `launch_gate::cause_page` と同じ表。
 */
export function fallbackNotice(cause: string | null | undefined): FallbackNotice | null {
	switch (cause) {
		case 'leftTight':
			return {
				title: '運用モードのまま残っています',
				message:
					'サービスが、profile のファイルの保護を外せないまま止まりました（終了コード 5）。このアプリでは Hub を起動しません。「設定モードに戻す」で保護を外します。それで戻らないときは、管理者の許可（ユーザーアカウント制御）を求めて直します。',
				offerRestore: true
			};
		case 'operationStopped':
			return {
				title: '運用モード（停止中）です',
				message:
					'サービス（運用モード）が止まっています。profile のファイルは保護されたままなので、このアプリでは Hub を起動しません。設定を変えるときは「設定モードに戻す」を押します。',
				offerRestore: true
			};
		case 'cannotOperate':
			return {
				title: '運用モードにできませんでした',
				message:
					'profile のファイルを保護できなかったため、運用モードに切り替えませんでした（終了コード 4）。ファイルは変わっておらず、設定モードのままです。理由はサービスのログにあります。トレイの「再試行」で、このアプリで Hub を起動し直します。',
				offerRestore: false
			};
		case 'profileUnusable':
			return {
				title: 'profile のフォルダを使えません',
				message:
					'profile のフォルダがリンク（ジャンクション）になっているなど、安全に使えない状態です。管理者に確かめてもらい、直した後にトレイの「再試行」を押します。',
				offerRestore: false
			};
		case 'serviceLost':
			return {
				title: 'サービスが止まりました',
				message:
					'接続していたサービスが止まりました。このアプリでは自動で Hub を起動しません。トレイの「再試行」で状態を確かめ直します。',
				offerRestore: false
			};
		default:
			return null;
	}
}

/**
 * [設定モードに戻す] を押せるか（純関数）。ローカルシェルで、理由が締まったまま止まっている
 * ときだけ。Operators でなくても押せる（戻らなければ管理者の許可で修復する）。切替中は不可。
 */
export function canRestoreSettingMode(
	input: HostSwitchGateInput,
	cause: string | null | undefined
): boolean {
	if (!input.isLocalShell || input.switching) return false;
	return fallbackNotice(cause)?.offerRestore === true;
}
