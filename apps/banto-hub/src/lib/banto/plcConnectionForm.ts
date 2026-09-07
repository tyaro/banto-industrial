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
 * 4. **プロトコルに応じた既定ワード順の追従**（2026-09-08 オーナー決定、
 *    issue #325 の修正 - ポートの追従（上記3）と同じ「未編集なら追従、
 *    明示的に編集した後は上書きしない」方式）: {@link DEFAULT_WORD_ORDERS}・
 *    {@link defaultWordOrderFor}・{@link isDefaultWordOrderForProtocol}。
 *    `modbus-tcp` = `high_low`（Modbus/IEEE慣習に統一 - 収集ポーリング経路と
 *    read-on-demand/書き込み経路とでワード順の解釈が食い違っていた実バグの
 *    是正）、`slmp` = `low_high`（MELSEC標準、従来どおり変更無し）。
 */
import type { PlcConnection, PlcConnectionInput, PlcProtocol, WordOrder } from './tagRegistryAdmin';
import { nextSequentialName } from './sequentialName';

/**
 * S1: `banto_tags::plc_connection::POSTGRES_PROTOCOL`/
 * `tagRegistryAdmin.ts::POSTGRES_PROTOCOL`と同じ値をここに複製する
 * （`tagTreeContextMenu.ts`の「依存ゼロ」方針と同じ理由 - このモジュールは
 * `PlcConnection`等を**型としてのみ** import しており、`tagRegistryAdmin.ts`
 * を値として import すると、そちらが `@banto/admin-core`/`./setup`/
 * `./deferredDelete.svelte`（Svelte 5 rune を使う`.svelte.ts`）をトップ
 * レベルで import するせいで、このモジュール単体のテスト
 * （`plcConnectionForm.test.ts`、`vi.mock`無し）が`ReferenceError: $state
 * is not defined`で落ちる - `tagRegistryAdmin.test.ts`のdoc comment
 * 「vitest 制約の回避について」参照）。
 */
const POSTGRES_PROTOCOL: PlcProtocol = 'postgres';

// "virtual" is intentionally NOT offered here — the two virtual connections
// (calc/mem) are auto-provisioned by the backend, not created through this
// form (plc-connections/+page.svelte の元コメントを踏襲)。
//
// S1（docs/banto-hub-external-db-design.md §4.1・§6-4）: 'postgres'
// （PostgreSQL への DB Source 接続）はここに追加する - PLC ではないが、
// 案A（既存3階層の流用、design §3.1）どおり同じ plc_connections
// テーブル・同じ作成 Drawer から作る。
export const PROTOCOL_OPTIONS: { value: PlcProtocol; label: string }[] = [
	{ value: 'modbus-tcp', label: 'Modbus TCP' },
	{ value: 'slmp', label: 'SLMP（MELSEC）' },
	{ value: 'postgres', label: 'PostgreSQL（DB Source）' }
];

/**
 * プロトコルごとの既定ポート。上のモジュール doc comment に根拠を記載。
 * `virtual` はここに含めない（新規作成の選択肢に出さないプロトコルであり、
 * ポートの意味を持たない接続のため）。`postgres` = 5432 は PostgreSQL の
 * 標準ポート（design §4.2）。
 */
export const DEFAULT_PORTS: Record<'modbus-tcp' | 'slmp' | 'postgres', number> = {
	'modbus-tcp': 502,
	slmp: 5007,
	postgres: 5432
};

function hasDefaultPort(protocol: PlcProtocol): protocol is keyof typeof DEFAULT_PORTS {
	return protocol === 'modbus-tcp' || protocol === 'slmp' || protocol === POSTGRES_PROTOCOL;
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
 * プロトコルごとの既定ワード順（2026-09-08 オーナー決定、issue #325 の修正）。
 * `modbus-tcp` = `high_low`（Modbus/IEEE 慣習に統一 - 収集ポーリング経路が
 * 常に HighLow 固定で読んでいたのに read-on-demand/書き込み経路は列の値
 * （旧既定 `low_high`）を読んでいたため、同じ u32/f32 タグが経路によって
 * ワード反転して食い違う実バグがあった。migration で既存 modbus-tcp 行も
 * `high_low` へ backfill 済み）。`slmp` = `low_high`（MELSEC標準、従来どおり
 * 変更無し）。`virtual`/`postgres` はワード順を持たない
 * （{@link hasDefaultPort}と同じ形の判定 - `tagRegistryAdmin.ts::hasWordOrder`
 * 参照）。
 */
export const DEFAULT_WORD_ORDERS: Record<'modbus-tcp' | 'slmp', WordOrder> = {
	'modbus-tcp': 'high_low',
	slmp: 'low_high'
};

function hasDefaultWordOrder(protocol: PlcProtocol): protocol is keyof typeof DEFAULT_WORD_ORDERS {
	return protocol === 'modbus-tcp' || protocol === 'slmp';
}

/** `protocol` の既定ワード順。既定を持たないプロトコル（virtual/postgres）は `undefined`。 */
export function defaultWordOrderFor(protocol: PlcProtocol): WordOrder | undefined {
	return hasDefaultWordOrder(protocol) ? DEFAULT_WORD_ORDERS[protocol] : undefined;
}

/**
 * `wordOrder`（フォームの現在値）が `protocol` の既定ワード順と一致しているか。
 * `isDefaultPortForProtocol` と同じ役割 - `ConnectionDrawer.svelte` はこれを
 * 使って、フォームを開いた時点やプロトコル切り替え時に「ワード順追従」を
 * 続けてよいかどうかの初期値（`wordOrderTouched`）を決める。
 */
export function isDefaultWordOrderForProtocol(
	wordOrder: WordOrder,
	protocol: PlcProtocol
): boolean {
	const def = defaultWordOrderFor(protocol);
	return def !== undefined && wordOrder === def;
}

/**
 * フォームを開いた時点で「ユーザーがワード順を明示的に選んだ状態」とみなすか
 * （`ConnectionDrawer.svelte` の `wordOrderTouched` の初期値）。`true` を返すと
 * 以後プロトコルを切り替えてもワード順は追従しなくなる。
 *
 * `virtual`/`postgres` は**ワード順の欄がそもそもフォームに出ない**
 * （`hasWordOrder` 参照）ので、保存されている値が何であれ「ユーザーは触って
 * いない」= `false` とする。ここを
 * `!isDefaultWordOrderForProtocol(...)` だけで決めると、既定を持たない
 * プロトコルでは常に `true` になり、そこから `modbus-tcp` へ切り替えたときに
 * 既定ワード順の追従が効かない - 例えば既存の postgres 接続（DB 上は
 * `word_order='low_high'` 固定）を Modbus に変更すると、ユーザーがワード順を
 * 触らない限り `low_high` のまま保存され、Modbus の既定 `high_low`
 * （2026-09-08 オーナー決定）と食い違う。
 */
export function initialWordOrderTouched(wordOrder: WordOrder, protocol: PlcProtocol): boolean {
	if (defaultWordOrderFor(protocol) === undefined) return false;
	return !isDefaultWordOrderForProtocol(wordOrder, protocol);
}

/**
 * TAG-UX-8: 新規作成フォームの名前プリフィル。`prefix`（既定 `"connection"`）
 * に続く数字部分だけを見て、`existingNames` に含まれない**最小の正整数**を
 * 選ぶ（「次の連番」＝最大値+1 ではなく、歯抜けがあれば埋める - 実装指示の
 * 例「connection1, connection3 があれば connection2」のとおり）。
 * `prefix` 部分の大文字小文字や前後の記号は区別しない緩い一致はしない
 * （`^${prefix}(\d+)$` の厳密一致 — `connection1-old` のような接尾辞付きは
 * 無視し、番号として扱わない）。
 *
 * T18-6b: 採番ロジック自体は {@link nextSequentialName}（`sequentialName.ts`）
 * へ切り出し、収集グループ側（`collectionGroupForm.ts::nextGroupName`）と
 * 共有している。
 *
 * 修正1（実機で再現した不具合、2026-08-31 オーナー報告）: `pendingNames`
 * （pending queue 内の未適用の `plc_connections.create` 分の名前 -
 * `pendingCreateNames.ts::pendingCreateNames` で抽出したもの）も衝突候補
 * として受け取れるよう拡張した。既存の呼び出し（`existingNames`/`prefix`
 * のみ渡す形）は `pendingNames` が既定 `[]` なので無改変で通る
 * （`plcConnectionForm.test.ts` の既存ケースはそのまま）。収集グループ側
 * （`collectionGroupForm.ts::nextGroupName`）と同じ理由 - 詳細はそちらの
 * モジュール doc comment 参照。
 */
export function nextConnectionName(
	existingNames: readonly string[],
	prefix = 'connection',
	pendingNames: readonly string[] = []
): string {
	return nextSequentialName([...existingNames, ...pendingNames], prefix);
}

/**
 * S1（docs/banto-hub-external-db-design.md §4.1、`crates/banto-tags/src
 * /plc_connection.rs`の`MAX_DATABASE_LEN`/`MAX_USERNAME_LEN`）: サーバー側
 * バリデーションの上限文字数をそのままミラーする。{@link validatePostgresFields}
 * が使う。
 */
export const MAX_DATABASE_LEN = 128;
export const MAX_USERNAME_LEN = 128;

/** 編集フォーム状態（作成/編集共通）。数値入力は文字列で保持し、空欄=未設定。 */
export interface PlcConnectionFormState {
	name: string;
	protocol: PlcProtocol;
	host: string;
	port: string;
	unitId: string;
	enabled: boolean;
	simulation: boolean;
	wordOrder: WordOrder;
	/** S1: `protocol === 'postgres'` のときだけ意味を持つ。DB 名。 */
	database: string;
	/** S1: `protocol === 'postgres'` のときだけ意味を持つ。DB ユーザー名。 */
	username: string;
	/**
	 * S1: パスワード欄の生の入力値。**編集フォームでは絶対にプリフィル
	 * しない** - `connectionToForm` は常に空文字列を返す（保存済みの
	 * パスワードを一度も画面に出さないための設計、
	 * `PlcConnection::password`の doc comment「外部呼び出し元へ決して
	 * シリアライズしない」と同じ理由）。空欄のまま送信すると
	 * `passwordForSubmit` が「現在のパスワードを変更しない」（update）/
	 * 「パスワード無し」（create）に変換する。
	 */
	password: string;
	/**
	 * S1: 「パスワードを消去する」チェックボックス（編集フォームのみ表示）。
	 * `true` のとき `passwordForSubmit` は `password` 欄の中身を無視して
	 * 常に空文字列（=消去）を送る。
	 */
	clearPassword: boolean;
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
		// 2026-09-08 オーナー決定（issue #325 の修正）: プロトコルごとの既定
		// ワード順（{@link DEFAULT_WORD_ORDERS}）に一致させる - ここでの既定
		// protocol は 'modbus-tcp' なので、その既定 'high_low' を使う。
		wordOrder: DEFAULT_WORD_ORDERS['modbus-tcp'],
		database: '',
		username: '',
		password: '',
		clearPassword: false
	};
}

/**
 * 保存済み接続をフォーム状態へ変換する（編集フォームの初期値）。
 * S1: `password`は常に空文字列で返す（上の`PlcConnectionFormState::password`
 * の doc comment参照 - 保存済みのパスワードを画面に出さない）。
 */
export function connectionToForm(c: PlcConnection): PlcConnectionFormState {
	return {
		name: c.name,
		protocol: c.protocol,
		host: c.host,
		port: String(c.port),
		unitId: String(c.unitId),
		enabled: c.enabled,
		simulation: c.simulation,
		wordOrder: c.wordOrder,
		database: c.database ?? '',
		username: c.username ?? '',
		password: '',
		clearPassword: false
	};
}

/**
 * S1: `form.password`/`form.clearPassword` から`PlcConnectionInput::password`
 * の tri-state を組み立てる - mirrors
 * `banto_tags::PlcConnectionInput::password`のdoc comment（create/updateで
 * 意味が違う点も含め）。
 *
 * - `clearPassword` が `true`: 常に `""`（消去。update専用の意味だが、
 *   create側は`""`も「パスワード無し」として扱うので同じ値で安全）。
 * - `password` が空文字列（未入力）: `undefined` を返す - update では
 *   「現在のパスワードを変更しない」、create では「パスワード無し」と、
 *   どちらの意味でも安全な既定値になる。
 * - それ以外（非空文字列を入力）: その値をそのまま返す（置き換え/新規設定）。
 */
export function passwordForSubmit(
	form: Pick<PlcConnectionFormState, 'password' | 'clearPassword'>
): string | undefined {
	if (form.clearPassword) return '';
	return form.password === '' ? undefined : form.password;
}

/**
 * S1（docs/banto-hub-external-db-design.md §4.1）: クライアント側の事前
 * 検証 - サーバー側`validate_plc_connection_input`の postgres 必須ルール
 * （`database`/`username`が trim 後空でないこと、`MAX_DATABASE_LEN`/
 * `MAX_USERNAME_LEN`以内であること）だけをミラーする。他プロトコルでは
 * 常に空オブジェクトを返す（このモジュールは非 postgres 用の禁止ルール
 * ―`database`/`username`/`password`は postgres 以外では指定不可― を検証
 * しない。`formToConnectionInput`がそもそも非 postgres ではこれらの
 * フィールドを送らないため、フォーム側でこの逆ルールに違反しようがない）。
 * メッセージは`crates/banto-tags/src/support.rs`の
 * `required_message`/`max_length_message`と一字一句揃える。
 */
export function validatePostgresFields(
	form: Pick<PlcConnectionFormState, 'protocol' | 'database' | 'username'>
): Record<string, string> {
	const errors: Record<string, string> = {};
	if (form.protocol !== POSTGRES_PROTOCOL) return errors;

	const database = form.database.trim();
	if (database === '') {
		errors.database = '必須項目です';
	} else if (database.length > MAX_DATABASE_LEN) {
		errors.database = `${MAX_DATABASE_LEN}文字以内で入力してください`;
	}

	const username = form.username.trim();
	if (username === '') {
		errors.username = '必須項目です';
	} else if (username.length > MAX_USERNAME_LEN) {
		errors.username = `${MAX_USERNAME_LEN}文字以内で入力してください`;
	}

	return errors;
}

/**
 * フォーム状態を API 入力（`PlcConnectionInput`）へ変換する。S1: `protocol
 * === 'postgres'`のときだけ`database`/`username`/`password`を足す - 他
 * プロトコルではこの3フィールドを一切送らない（サーバーが非 postgres での
 * 指定を拒否するため、そもそも送らないのが最も安全 -
 * `validatePostgresFields`のdoc comment参照）。
 */
export function formToConnectionInput(form: PlcConnectionFormState): PlcConnectionInput {
	const base: PlcConnectionInput = {
		name: form.name,
		protocol: form.protocol,
		host: form.host,
		port: Number(form.port),
		unitId: Number(form.unitId),
		enabled: form.enabled,
		simulation: form.simulation,
		wordOrder: form.wordOrder
	};
	if (form.protocol !== POSTGRES_PROTOCOL) return base;
	return {
		...base,
		database: form.database,
		username: form.username,
		password: passwordForSubmit(form)
	};
}
