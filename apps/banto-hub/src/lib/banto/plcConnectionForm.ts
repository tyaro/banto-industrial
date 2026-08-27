/**
 * T18-6a（TAG-UX-7/TAG-UX-8、2026-08-27 オーナー決定「PLC接続の作成／再設定を
 * Drawer に寄せる」）: `ConnectionDrawer.svelte`（`plc-connections/+page.svelte`
 * と将来のタグツリー右クリック双方から使う共通部品）が必要とする、依存ゼロの
 * 純関数・定数・フォーム状態の型だけを切り出す。`tagFormLayout.ts` と同じ方針
 * — Svelte 側は `$state`/DOM 組み立てに専念させ、ここに置く関数はスナップ
 * ショット値だけを引数に取り、テストしやすく保つ。
 *
 * 本モジュールが担う3つの役割:
 *
 * 1. **フォーム状態の組み立て**（旧 `plc-connections/+page.svelte` が
 *    ページ内に持っていた `FormState`/`blankForm`/`formFromConnection`/
 *    `toInput` を無改変で移設 — 検証・既定値を一切変えていない）。
 * 2. **新規作成時の連番名プリフィル**（TAG-UX-8「空欄で出さず
 *    `connection1` のように、既存の接続名と衝突しない最小の連番を初期値に
 *    入れる」）: {@link nextConnectionName}。
 * 3. **プロトコルに応じた既定ポートの追従**（実装指示「プロトコルを
 *    切り替えたときポートが未編集（既定値のまま）なら追従させ、ユーザーが
 *    明示的に編集した後は勝手に上書きしないこと」）: {@link DEFAULT_PORTS}・
 *    {@link defaultPortFor}・{@link isDefaultPortForProtocol}。
 *
 *    既定値の根拠: `modbus-tcp` = 502 は本ページの旧実装が使っていた値
 *    （デバッグしやすさを優先した既存の選定、docs/plan.md I2 の判断を踏襲）。
 *    `slmp` = 5007 は `crates/banto-plc/src/slmp/mod.rs`
 *    `SlmpConfig::default()` が使う値（同ファイルのコメント曰く
 *    「SLMPのポートに普遍的な既定は無いが、5007はラップ元クレートの
 *    サンプル値でありバイナリ4Eフレームでよく使われる」）。テスト環境の
 *    実機 R08ENCPU は `192.168.11.200:5200` だが、これは実機固有の値であり
 *    既定値には採用しない（実装指示のとおり）。
 */
import type {
	PlcConnection,
	PlcConnectionInput,
	PlcProtocol,
	SlmpWordOrder
} from './tagRegistryAdmin';

// "virtual" is intentionally NOT offered here — the two virtual connections
// (calc/mem) are auto-provisioned by the backend, not created through this
// form (plc-connections/+page.svelte の元コメントを踏襲)。
export const PROTOCOL_OPTIONS: { value: PlcProtocol; label: string }[] = [
	{ value: 'modbus-tcp', label: 'Modbus TCP' },
	{ value: 'slmp', label: 'SLMP（MELSEC）' }
];

/**
 * プロトコルごとの既定ポート。上のモジュール doc comment に根拠を記載。
 * `virtual` はここに含めない（新規作成の選択肢に出さないプロトコルであり、
 * ポートの意味を持たない接続のため）。
 */
export const DEFAULT_PORTS: Record<'modbus-tcp' | 'slmp', number> = {
	'modbus-tcp': 502,
	slmp: 5007
};

function hasDefaultPort(protocol: PlcProtocol): protocol is keyof typeof DEFAULT_PORTS {
	return protocol === 'modbus-tcp' || protocol === 'slmp';
}

/** `protocol` の既定ポート。`virtual` など既定を持たないプロトコルは `undefined`。 */
export function defaultPortFor(protocol: PlcProtocol): number | undefined {
	return hasDefaultPort(protocol) ? DEFAULT_PORTS[protocol] : undefined;
}

/**
 * `port`（フォームの文字列値）が `protocol` の既定ポートと一致しているか。
 * 「まだユーザーが明示的に編集していない（＝既定値のまま）」の判定に使う —
 * `ConnectionDrawer.svelte` はこれを使って、フォームを開いた時点や
 * プロトコル切り替え時に「ポート追従」を続けてよいかどうかの初期値
 * （`portTouched`）を決める。
 */
export function isDefaultPortForProtocol(port: string, protocol: PlcProtocol): boolean {
	const def = defaultPortFor(protocol);
	return def !== undefined && port === String(def);
}

/**
 * TAG-UX-8: 新規作成フォームの名前プリフィル。`prefix`（既定 `"connection"`）
 * に続く数字部分だけを見て、`existingNames` に含まれない**最小の正整数**を
 * 選ぶ（「次の連番」＝最大値+1 ではなく、歯抜けがあれば埋める - 実装指示の
 * 例「connection1, connection3 があれば connection2」のとおり）。
 * `prefix` 部分の大文字小文字や前後の記号は区別しない緩い一致はしない
 * （`^${prefix}(\d+)$` の厳密一致 — `connection1-old` のような接尾辞付きは
 * 無視し、番号として扱わない）。
 */
export function nextConnectionName(
	existingNames: readonly string[],
	prefix = 'connection'
): string {
	const escapedPrefix = prefix.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	const pattern = new RegExp(`^${escapedPrefix}(\\d+)$`);
	const used = new Set<number>();
	for (const name of existingNames) {
		const match = pattern.exec(name);
		if (match) used.add(Number(match[1]));
	}
	let n = 1;
	while (used.has(n)) n++;
	return `${prefix}${n}`;
}

/** 編集フォーム状態（作成/編集共通）。数値入力は文字列で保持し、空欄=未設定。 */
export interface PlcConnectionFormState {
	name: string;
	protocol: PlcProtocol;
	host: string;
	port: string;
	unitId: string;
	enabled: boolean;
	simulation: boolean;
	wordOrder: SlmpWordOrder;
}

/**
 * 新規作成フォームの初期値。`name` はここでは空のまま返す —
 * TAG-UX-8 の連番プリフィルは `existingNames` が要る（このモジュールでは
 * 副作用なく完結させたいので）呼び出し側（`ConnectionDrawer.svelte`）が
 * `blankForm()` の直後に `nextConnectionName()` の結果を代入する。
 */
export function blankConnectionForm(): PlcConnectionFormState {
	return {
		name: '',
		// バックエンドの既定（PlcConnectionPayload の default_plc_protocol）
		// と一致させる。
		protocol: 'modbus-tcp',
		host: '',
		port: String(DEFAULT_PORTS['modbus-tcp']),
		unitId: '1',
		enabled: true,
		simulation: false,
		// P3-b（監査指摘 2026-08-12）: バックエンドの既定
		// （default_plc_word_order / SlmpConfig::default().word_order）と
		// 一致させる。
		wordOrder: 'low_high'
	};
}

/** 保存済み接続をフォーム状態へ変換する（編集フォームの初期値）。 */
export function connectionToForm(c: PlcConnection): PlcConnectionFormState {
	return {
		name: c.name,
		protocol: c.protocol,
		host: c.host,
		port: String(c.port),
		unitId: String(c.unitId),
		enabled: c.enabled,
		simulation: c.simulation,
		wordOrder: c.wordOrder
	};
}

/** フォーム状態を API 入力（`PlcConnectionInput`）へ変換する。 */
export function formToConnectionInput(form: PlcConnectionFormState): PlcConnectionInput {
	return {
		name: form.name,
		protocol: form.protocol,
		host: form.host,
		port: Number(form.port),
		unitId: Number(form.unitId),
		enabled: form.enabled,
		simulation: form.simulation,
		wordOrder: form.wordOrder
	};
}
