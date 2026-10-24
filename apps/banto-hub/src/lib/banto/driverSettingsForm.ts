/**
 * R2b-6c（docs/banto-hub-driver-ui-design.md §6.1・§6.2）: 機器のドライバの接続設定を
 * `GET /api/drivers` のスキーマ（{@link DriverSettingField}）から組み立てる、依存の無い
 * 純関数。`DriverSettingsForm.svelte` は描画だけを持ち、判断（既定値・保存前の検査・
 * 送る `settings` の組み立て・ドライバを替えたときの引き継ぎ・サーバーの項目エラーの写し）は
 * ここに置く。
 *
 * - フォームの値は欄の種類ごとに文字列・真偽・{@link SecretFormValue} で持つ。数値の欄も
 *   文字列で持ち（空欄 = 未入力）、送るときに数値へ直す。
 * - `settings` の中は `banto-driver-api` の serde の形（snake_case）のまま。外側の
 *   camelCase と一律に変換しない。
 * - 検査の規則と文言はサーバー（`banto_tags::settings_json::check_field_value`）と同じ。
 *   UI で先に断るだけで、最終の判定はサーバー。
 */
import type { DriverSettingField } from './tagRegistryAdmin';

/** 応答の秘密のキーの値 `{"$secret": "set" | "unreadable"}` か（`tagRegistryAdmin.ts::isSecretStateWire` と同じ。型だけ読むため複製）。 */
function secretWireState(value: unknown): 'set' | 'unreadable' | null {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
	const keys = Object.keys(value);
	if (keys.length !== 1 || keys[0] !== '$secret') return null;
	const state = (value as { $secret: unknown }).$secret;
	return state === 'unreadable' ? 'unreadable' : 'set';
}

/** 秘密の欄の操作: 保存済みを保つ・新しい値に変える・消す。 */
export type SecretMode = 'keep' | 'change' | 'clear';

/** 保存済みの秘密の状態（`$secret`）。値が無ければ `none`。 */
export type SecretStateName = 'none' | 'set' | 'unreadable';

/** 秘密の欄のフォームの値。`text` は「変える」で入れる新しい値（保存済みは決して入れない）。 */
export interface SecretFormValue {
	mode: SecretMode;
	text: string;
	/** 保存済みの状態。新規作成と、値が無い欄は `none`（このときは「保つ」「消す」を出さない）。 */
	state: SecretStateName;
}

export type DriverFieldValue = string | boolean | SecretFormValue;
export type DriverFormValues = Record<string, DriverFieldValue>;

/** ドライバの欄の入力の状態。`touched` は利用者が触った欄（ドライバを替えたときの引き継ぎに使う）。 */
export interface DriverFormState {
	values: DriverFormValues;
	touched: Record<string, boolean>;
}

export const REQUIRED_MESSAGE = '必須です';

export function isSecretFormValue(value: unknown): value is SecretFormValue {
	return (
		typeof value === 'object' &&
		value !== null &&
		'mode' in value &&
		'text' in value &&
		'state' in value
	);
}

function blankSecret(): SecretFormValue {
	return { mode: 'change', text: '', state: 'none' };
}

/** スキーマの既定で作る 1 欄の初期値。 */
export function defaultFieldValue(field: DriverSettingField): DriverFieldValue {
	switch (field.kind.type) {
		case 'bool':
			return field.default === true;
		case 'secret':
			return blankSecret();
		case 'choice': {
			if (typeof field.default === 'string') return field.default;
			return field.required ? (field.kind.options[0]?.value ?? '') : '';
		}
		default:
			return field.default === null || field.default === undefined ? '' : String(field.default);
	}
}

/** スキーマの既定で埋めたフォームの値。 */
export function defaultDriverValues(fields: readonly DriverSettingField[]): DriverFormValues {
	const values: DriverFormValues = {};
	for (const field of fields) values[field.key] = defaultFieldValue(field);
	return values;
}

export function defaultDriverState(fields: readonly DriverSettingField[]): DriverFormState {
	return { values: defaultDriverValues(fields), touched: {} };
}

/**
 * 保存済みの `settings` からフォームの値を作る。`settings` に無い欄はスキーマの既定
 * （REST の `settings` は丸ごと置き換えで、省いた欄は既定に戻るため）。秘密の欄は
 * `{"$secret": "set" | "unreadable"}` を「設定済み（保つ）」の状態にし、値は決して持たない。
 */
export function driverStateFromSettings(
	fields: readonly DriverSettingField[],
	settings: Record<string, unknown> | null | undefined
): DriverFormState {
	const values: DriverFormValues = {};
	const source = settings ?? {};
	for (const field of fields) {
		const raw = source[field.key];
		if (field.kind.type === 'secret') {
			const state = secretWireState(raw);
			if (state !== null) {
				values[field.key] = { mode: 'keep', text: '', state };
			} else {
				values[field.key] = blankSecret();
			}
			continue;
		}
		if (raw === undefined || raw === null) {
			values[field.key] = defaultFieldValue(field);
		} else if (field.kind.type === 'bool') {
			values[field.key] = raw === true;
		} else {
			values[field.key] = String(raw);
		}
	}
	return { values, touched: {} };
}

/**
 * ドライバを替えたときの値。**同じキーで種類が同じ、かつ利用者が触った欄の値だけ**を引き継ぎ
 * （選択肢の欄は、新しい選択肢に無い値は引き継がない）、他は新しいスキーマの既定で埋め直す。
 * 秘密の欄は引き継がない（別の機器の資格情報を持ち越さない）。`previous` が `null`
 * （ホストの接続から替えた）なら全部既定。
 */
export function switchDriverState(
	previous: { fields: readonly DriverSettingField[]; state: DriverFormState } | null,
	nextFields: readonly DriverSettingField[]
): DriverFormState {
	const next = defaultDriverState(nextFields);
	if (previous === null) return next;
	for (const field of nextFields) {
		if (field.kind.type === 'secret') continue;
		const old = previous.fields.find((f) => f.key === field.key);
		if (!old || old.kind.type !== field.kind.type) continue;
		if (!previous.state.touched[field.key]) continue;
		const value = previous.state.values[field.key];
		if (value === undefined || isSecretFormValue(value)) continue;
		if (field.kind.type === 'choice') {
			if (typeof value !== 'string' || !field.kind.options.some((o) => o.value === value)) continue;
		}
		next.values[field.key] = value;
		next.touched[field.key] = true;
	}
	return next;
}

const INTEGER_RE = /^[+-]?\d+$/;

/**
 * 保存前の検査（種類・必須・min/max・max_len だけ。サーバーと同じ規則と文言）。
 * 返すのは欄の key → 文言。空なら通った。`isCreate` は新規作成（「保つ」が無い）か。
 */
export function validateDriverValues(
	fields: readonly DriverSettingField[],
	values: DriverFormValues
): Record<string, string> {
	const errors: Record<string, string> = {};
	for (const field of fields) {
		const value = values[field.key];
		const message = validateField(field, value);
		if (message !== null) errors[field.key] = message;
	}
	return errors;
}

function validateField(
	field: DriverSettingField,
	value: DriverFieldValue | undefined
): string | null {
	const kind = field.kind;
	switch (kind.type) {
		case 'bool':
			return null;
		case 'secret': {
			const secret = isSecretFormValue(value) ? value : blankSecret();
			if (secret.state === 'none') {
				return field.required && secret.text === '' ? REQUIRED_MESSAGE : null;
			}
			if (secret.mode === 'change') {
				return secret.text === '' ? '新しい値を入力してください' : null;
			}
			if (secret.mode === 'clear' && field.required) return REQUIRED_MESSAGE;
			return null;
		}
		case 'text': {
			const text = typeof value === 'string' ? value : '';
			if (field.required && text.trim() === '') return REQUIRED_MESSAGE;
			if (kind.max_len !== null && [...text].length > kind.max_len) {
				return `${kind.max_len} 文字以下にしてください`;
			}
			return null;
		}
		case 'host': {
			const text = typeof value === 'string' ? value : '';
			return field.required && text.trim() === '' ? REQUIRED_MESSAGE : null;
		}
		case 'integer': {
			const text = (typeof value === 'string' ? value : '').trim();
			if (text === '') {
				return field.required && (field.default === null || field.default === undefined)
					? REQUIRED_MESSAGE
					: null;
			}
			if (!INTEGER_RE.test(text) || !Number.isSafeInteger(Number(text))) {
				return '整数で指定してください';
			}
			const n = Number(text);
			const { min, max } = kind;
			if (min !== null && max !== null && (n < min || n > max))
				return `${min}〜${max} で指定してください`;
			if (min !== null && max === null && n < min) return `${min} 以上で指定してください`;
			if (min === null && max !== null && n > max) return `${max} 以下で指定してください`;
			return null;
		}
		case 'choice': {
			const text = typeof value === 'string' ? value : '';
			if (text === '') return field.required ? REQUIRED_MESSAGE : null;
			if (!kind.options.some((o) => o.value === text)) {
				return `${kind.options.map((o) => o.value).join(', ')} のいずれかを指定してください`;
			}
			return null;
		}
		default:
			return null;
	}
}

/**
 * 送る `settings` を組み立てる（`settings` は丸ごと置き換え）。
 * - Integer は数値で送る。空欄は送らない（スキーマの既定になる）。
 * - Text・Host は文字列で送る（Host は前後の空白を落とす）。Choice の空欄は送らない。
 * - Secret: 「変えない」= キーを送らない、「消す」= `null`、「変える」= 入力した文字列。
 *   保存済みの値が無い欄（新規作成を含む）は、入力があれば文字列、無ければ送らない。
 */
export function buildDriverSettings(
	fields: readonly DriverSettingField[],
	values: DriverFormValues
): Record<string, unknown> {
	const settings: Record<string, unknown> = {};
	for (const field of fields) {
		const value = values[field.key];
		switch (field.kind.type) {
			case 'bool':
				settings[field.key] = value === true;
				break;
			case 'secret': {
				const secret = isSecretFormValue(value) ? value : blankSecret();
				if (secret.state === 'none') {
					if (secret.text !== '') settings[field.key] = secret.text;
				} else if (secret.mode === 'clear') {
					settings[field.key] = null;
				} else if (secret.mode === 'change') {
					settings[field.key] = secret.text;
				}
				break;
			}
			case 'integer': {
				const text = (typeof value === 'string' ? value : '').trim();
				if (text !== '') settings[field.key] = Number(text);
				break;
			}
			case 'choice': {
				const text = typeof value === 'string' ? value : '';
				if (text !== '') settings[field.key] = text;
				break;
			}
			case 'host':
				settings[field.key] = (typeof value === 'string' ? value : '').trim();
				break;
			default:
				settings[field.key] = typeof value === 'string' ? value : '';
		}
	}
	return settings;
}

function camelToSnake(name: string): string {
	return name.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
}

/**
 * サーバーの項目エラーを欄の key へ写す。`settings.<key>` と、サーバーが camelCase で返す
 * `unitId`・`wordOrder`・`cpuType`（→ `unit_id` など）と、そのままの `<key>` を欄に当てる。
 * 当たらないもの（`name`・`protocol` など）は `other` に残す。
 */
export function mapSettingsFieldErrors(
	keys: readonly string[],
	fieldErrors: Record<string, string>
): { fields: Record<string, string>; other: Record<string, string> } {
	const fields: Record<string, string> = {};
	const other: Record<string, string> = {};
	for (const [field, message] of Object.entries(fieldErrors)) {
		const stripped = field.startsWith('settings.') ? field.slice('settings.'.length) : field;
		const key = [stripped, camelToSnake(stripped)].find((k) => keys.includes(k));
		if (key !== undefined) {
			fields[key] = fields[key] ? `${fields[key]} / ${message}` : message;
		} else {
			other[field] = message;
		}
	}
	return { fields, other };
}

/** 確認の画面に出す 1 行。 */
export interface DriverSummaryRow {
	label: string;
	text: string;
}

/**
 * 作成の確認（ウィザードの 3 つ目）に出す、ドライバの欄の一覧。Choice は選択肢の表示名、
 * Bool は はい/いいえ、Secret は値を出さず状態（入力あり・未入力・設定済み・消す）だけ。
 */
export function summarizeDriverValues(
	fields: readonly DriverSettingField[],
	values: DriverFormValues
): DriverSummaryRow[] {
	return fields.map((field) => {
		const value = values[field.key];
		const kind = field.kind;
		let text: string;
		if (kind.type === 'bool') {
			text = value === true ? 'はい' : 'いいえ';
		} else if (kind.type === 'secret') {
			const secret = isSecretFormValue(value) ? value : blankSecret();
			if (secret.state !== 'none' && secret.mode === 'keep') text = '設定済み（変えない）';
			else if (secret.mode === 'clear') text = '消す';
			else text = secret.text !== '' ? '入力あり' : '（未入力）';
		} else {
			const raw = typeof value === 'string' ? value : '';
			if (kind.type === 'choice') {
				text = kind.options.find((o) => o.value === raw)?.label ?? raw;
			} else {
				text = raw;
			}
			if (text === '') text = '（未入力）';
		}
		return { label: field.label, text };
	});
}
