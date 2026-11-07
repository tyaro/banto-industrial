/**
 * PLC の時計（表示・時計合わせの設定・手動の時計合わせ）の型と純関数。
 * H2（#1065、時計合わせ）と H3（#1068、時計の読取り）の Hub の REST に対応する
 * （`apps/banto-hub/core/src/rest/clock_sync.rs`、型は `core/src/clock_sync.rs` と
 * `core/src/clock_read.rs`、JSON は camelCase）。
 *
 * ここは HTTP を持たない（型・日本語の表示・フォームの検証だけ）。4 本の HTTP 関数
 * （`getClockSync`・`putClockSync`・`runClockSync`・`readPlcClock`）は、非公開の
 * `httpRequest` を使うため `tagRegistryAdmin.ts` にあり、型はここから `import type`
 * する。画面は `PlcClockSection.svelte`。
 *
 * - 時刻のうち `plcTime`・`pcTime`・`ClockSyncRecord.time` は **タイムゾーンなしの
 *   現地時刻**（`YYYY-MM-DDTHH:MM:SS`）で、変換せずに出す（{@link plcTimeText}）。
 *   `at` だけが UTC の RFC3339 で、画面の現地時刻にする（既存の `formatSqliteUtc`）。
 * - 差（`offsetSeconds`）は PLC の時刻 − Hub の PC の時刻。秒までの時刻同士なので
 *   ±1 秒の誤差がある（画面の注記）。
 */

import { ProviderError } from '@banto/admin-core';

export type ClockSchedule =
	{ kind: 'hourly'; minute: number } | { kind: 'daily'; hour: number; minute: number };

export interface ClockSyncSettings {
	enabled: boolean;
	schedule: ClockSchedule;
}

export type ClockReadResultKind =
	| 'ok'
	| 'unsupported'
	| 'rejected'
	| 'device'
	| 'verificationFailed'
	| 'unavailable'
	| 'unknown'
	| 'notOffered';

export type ClockSyncResultKind = ClockReadResultKind | 'busy';

export interface ClockReading {
	/** UTC RFC3339。 */
	at: string;
	result: ClockReadResultKind;
	code: number | null;
	message: string;
	/** `ok` のときだけ。タイムゾーンなしの現地時刻。 */
	plcTime: string | null;
	/** Hub の PC の現地時刻。 */
	pcTime: string;
	/** `plcTime` − `pcTime`（秒）。`ok` のときだけ。 */
	offsetSeconds: number | null;
}

export interface ClockReadView extends ClockReading {
	/** 直近の結果（5 秒以内）の使い回しか。 */
	cached: boolean;
}

export interface ClockSyncRecord {
	/** UTC RFC3339。 */
	at: string;
	trigger: 'schedule' | 'manual';
	/** 渡した現地時刻（`YYYY-MM-DDTHH:MM:SS`）。送っていなければ null。 */
	time: string | null;
	result: ClockSyncResultKind;
	code: number | null;
	message: string;
	before: ClockReading | null;
	after: ClockReading | null;
}

export interface ClockSyncView {
	settings: ClockSyncSettings;
	last: ClockSyncRecord | null;
}

/** 時計合わせの結果の短い名前。 */
export function clockSyncResultLabel(result: ClockSyncResultKind): string {
	switch (result) {
		case 'ok':
			return '成功';
		case 'unsupported':
			return 'この機種は非対応';
		case 'rejected':
			return 'ドライバが断った';
		case 'device':
			return '機器のエラー';
		case 'verificationFailed':
			return '確かめられなかった';
		case 'unavailable':
			return '送っていない（未接続など）';
		case 'unknown':
			return '結果不明';
		case 'notOffered':
			return 'このドライバは時計を扱わない';
		case 'busy':
			return '実行中';
		default:
			return String(result);
	}
}

/** 時計の読取りの結果の短い名前。 */
export function clockReadResultLabel(result: ClockReadResultKind): string {
	switch (result) {
		case 'ok':
			return '読めた';
		case 'unsupported':
			return 'この機種は非対応';
		case 'rejected':
			return 'ドライバが断った';
		case 'device':
			return '機器のエラー';
		case 'verificationFailed':
			return '値が時計として不正';
		case 'unavailable':
			return '送っていない（未接続など）';
		case 'unknown':
			return '結果不明';
		case 'notOffered':
			return 'このドライバは時計を扱わない';
		default:
			return String(result);
	}
}

/** `device` で終了コードがあれば `（終了コード 0x0055）`、それ以外は空文字。 */
export function deviceCodeSuffix(result: string, code: number | null): string {
	if (result !== 'device' || code === null || code === undefined) return '';
	return `（終了コード 0x${code.toString(16).toUpperCase().padStart(4, '0')}）`;
}

/** 差（秒、PLC − PC）。`+3 秒（PLC が進んでいる）`・`-75 秒…`・60 秒以上は分も出す。 */
export function formatOffset(seconds: number): string {
	if (seconds === 0) return '0 秒';
	const sign = seconds > 0 ? '+' : '-';
	const abs = Math.abs(seconds);
	const body = abs >= 60 ? `${Math.floor(abs / 60)} 分 ${abs % 60} 秒` : `${abs} 秒`;
	return `${sign}${body}（PLC が${seconds > 0 ? '進んでいる' : '遅れている'}）`;
}

/** 時刻表の要約。`毎時 5 分`・`毎日 03:30`。 */
export function scheduleSummary(schedule: ClockSchedule): string {
	if (schedule.kind === 'hourly') return `毎時 ${schedule.minute} 分`;
	const hh = String(schedule.hour).padStart(2, '0');
	const mm = String(schedule.minute).padStart(2, '0');
	return `毎日 ${hh}:${mm}`;
}

/** `YYYY-MM-DDTHH:MM:SS` → `YYYY-MM-DD HH:MM:SS`（タイムゾーン変換しない）。 */
export function plcTimeText(text: string): string {
	return text.replace('T', ' ');
}

/** 読取りの結果の 1 行（ok なら差、それ以外はラベルと終了コード）。 */
export function readingDiffText(reading: ClockReading): string {
	if (reading.result === 'ok' && reading.offsetSeconds !== null) {
		return formatOffset(reading.offsetSeconds);
	}
	return clockReadResultLabel(reading.result) + deviceCodeSuffix(reading.result, reading.code);
}

/** フォームの値（数値欄は未入力で null）。 */
export interface ClockSyncForm {
	enabled: boolean;
	kind: ClockSchedule['kind'];
	hour: number | null;
	minute: number | null;
}

export interface ClockSyncFormErrors {
	hour?: string;
	minute?: string;
}

export type ClockSyncFormResult =
	{ ok: true; settings: ClockSyncSettings } | { ok: false; errors: ClockSyncFormErrors };

function isIntInRange(value: unknown, min: number, max: number): value is number {
	return typeof value === 'number' && Number.isInteger(value) && value >= min && value <= max;
}

/** 時 0–23・分 0–59 の整数か確かめ、送る設定にする（毎時のときは時を見ない）。 */
export function validateClockSyncForm(form: ClockSyncForm): ClockSyncFormResult {
	const errors: ClockSyncFormErrors = {};
	if (form.kind === 'daily' && !isIntInRange(form.hour, 0, 23)) {
		errors.hour = '時は 0〜23 の整数で入力してください';
	}
	if (!isIntInRange(form.minute, 0, 59)) {
		errors.minute = '分は 0〜59 の整数で入力してください';
	}
	if (errors.hour || errors.minute) return { ok: false, errors };
	const minute = form.minute as number;
	const schedule: ClockSchedule =
		form.kind === 'daily'
			? { kind: 'daily', hour: form.hour as number, minute }
			: { kind: 'hourly', minute };
	return { ok: true, settings: { enabled: form.enabled, schedule } };
}

/** 保存済みの設定からフォームの初期値（毎時のときの時は 0 にしておく）。 */
export function settingsToForm(settings: ClockSyncSettings): ClockSyncForm {
	const s = settings.schedule;
	return {
		enabled: settings.enabled,
		kind: s.kind,
		hour: s.kind === 'daily' ? s.hour : 0,
		minute: s.minute
	};
}

/** 読み込んだ設定（基準値）とフォームが違うか。検証に通らない入力は違うとみなす。 */
export function isClockSyncFormDirty(base: ClockSyncSettings, form: ClockSyncForm): boolean {
	const parsed = validateClockSyncForm(form);
	if (!parsed.ok) return true;
	return JSON.stringify(parsed.settings) !== JSON.stringify(base);
}

/**
 * 404・503 は `{status:'unavailable', reason}`（`ErrorBody` でない）で返るので、
 * `httpRequest` の `mapErrorBody` で日本語の文言の `ProviderError` に写す。
 * `target` の `run` は POST（適用済みのカタログに無い接続）、`saved` は GET・PUT（保存済みに無い接続）。
 * それ以外は `undefined`（既定の扱いに委ねる）。
 */
export function mapClockUnavailable(
	target: 'run' | 'saved'
): (body: unknown, status: number) => Error | undefined {
	return (body, status) => {
		if (typeof body !== 'object' || body === null) return undefined;
		const b = body as { status?: unknown; reason?: unknown };
		if (b.status !== 'unavailable' || typeof b.reason !== 'string') return undefined;
		if (status === 404) {
			return new ProviderError({
				kind: 'other',
				message:
					target === 'run'
						? 'この接続は収集中の構成にありません（変更を適用してから実行してください）'
						: '接続が見つかりません'
			});
		}
		if (status === 503) {
			return new ProviderError({ kind: 'other', message: 'Hub の終了中のため実行しませんでした' });
		}
		return undefined;
	};
}
