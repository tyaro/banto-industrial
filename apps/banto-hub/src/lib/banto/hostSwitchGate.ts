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
	/** `GET /api/v1/status` の last_config_error。 */
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
 * 「サービスへ切り替えて開始」を有効にしてよいか。
 * Desktop または fallback（Offline）から Service へ。
 */
export function canSwitchToService(input: HostSwitchGateInput): boolean {
	if (!input.isLocalShell || !input.isAdmin || !input.canOperate) return false;
	if (input.switching) return false;
	if (input.view === 'service') return false;
	return isPreflightOk(input);
}

/**
 * 「サービスを停止してアプリで開く」を有効にしてよいか。
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
