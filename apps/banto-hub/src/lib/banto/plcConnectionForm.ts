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
 * 5. **取り込み口 `push` の設定**（R2a-2e、docs/banto-hub-ingest-design.md
 *    §3.2・§7.1）: `stale_timeout_ms` とブリッジの区画（読む `postgres` 接続・
 *    クエリ・列とキーの対応）のフォーム状態と `settings` の JSON の往復
 *    （{@link pushSettingsFromForm}・{@link pushFormFromSettings}）、保存前の
 *    検証（{@link validatePushFields}。§7.1 の量の見積もり {@link pushBridgeLoad}
 *    が上限の 8 割を超えたら保存の前に断る）、サーバーの項目エラーの写し
 *    （{@link mapPushFieldErrors}）、送り手向けの URL とスコープ
 *    （{@link ingestEndpoints}）。`push` は host / port / unitId / ワード順 /
 *    シミュレーションを持たない（サーバーが正規化する、§3.1）。
 */
import type {
	CpuType,
	PlcConnection,
	PlcConnectionInput,
	PlcProtocol,
	PushBridgeColumnWire,
	PushSettingsWire,
	WordOrder
} from './tagRegistryAdmin';
import { nextSequentialName } from './sequentialName';
import { bridgeLoad, type BridgeLoad } from './bridgeLoad';

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

/** R2a-2e: `tagRegistryAdmin.ts::PUSH_PROTOCOL` と同じ値（上の `POSTGRES_PROTOCOL` と同じ理由で複製）。 */
const PUSH_PROTOCOL: PlcProtocol = 'push';

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
	{ value: 'postgres', label: 'PostgreSQL（DB Source）' },
	// R2a-2e（docs/banto-hub-ingest-design.md §3.1）: 取り込み口。送り手（DB
	// ブリッジ・スクリプト）が Hub へ値を送る接続で、接続先を持たない。
	{ value: 'push', label: '取り込み（push・送り手が値を送る）' }
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
	/** #605: SLMP の CPU 種別。プロトコルを切り替えても値は残す。 */
	cpuType: CpuType;
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
	/**
	 * R2a-2e: `protocol === 'push'` のときだけ意味を持つ。送り手が黙ってから
	 * Down とみなすまで（ms、§4.3）。空欄は既定（{@link DEFAULT_STALE_TIMEOUT_MS}）。
	 */
	staleTimeoutMs: string;
	/** R2a-2e: push の DB ブリッジの区画を持つか（§3.2）。 */
	bridgeEnabled: boolean;
	/** R2a-2e: ブリッジが読む `postgres` 接続の id（`<select>` の値、未選択は `''`）。 */
	bridgeDbConnectionId: string;
	/** R2a-2e: ブリッジのクエリ（§3.2）。 */
	bridgeQueries: BridgeQueryFormState[];
}

/**
 * R2a-2e: ブリッジのクエリ 1 本のフォーム状態。列とキーの対応は 1 行に 1 つの
 * `列名=キー` で書く（{@link parseBridgeColumns}）。
 */
export interface BridgeQueryFormState {
	name: string;
	periodMs: number;
	sql: string;
	columnsText: string;
}

/** `stale_timeout_ms` の既定・下限・上限 - mirrors `banto_tags::push`（§4.3、Q3）。 */
export const DEFAULT_STALE_TIMEOUT_MS = 10_000;
export const MIN_STALE_TIMEOUT_MS = 1_000;
export const MAX_STALE_TIMEOUT_MS = 3_600_000;
/** `push` のキーの文字数の上限 - mirrors `banto_tags::MAX_PUSH_KEY_LEN`（§3.3）。 */
export const MAX_PUSH_KEY_LEN = 256;
/** ブリッジのクエリの名前の文字数の上限 - mirrors `banto_tags::MAX_BRIDGE_QUERY_NAME_LEN`。 */
export const MAX_BRIDGE_QUERY_NAME_LEN = 64;
/** 結果列名の上限 - mirrors `banto_tags::tag::MAX_DB_COLUMN_NAME_LEN`。 */
const MAX_DB_COLUMN_NAME_LEN = 63;
const DB_COLUMN_IDENTIFIER_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
/**
 * ブリッジのクエリの周期 - mirrors `banto_tags::ALLOWED_PERIOD_MS`
 * （`tagRegistryAdmin.ts::ALLOWED_PERIOD_MS` と同じ値。上の `POSTGRES_PROTOCOL`
 * と同じ理由で複製する）。
 */
export const BRIDGE_PERIOD_OPTIONS: readonly number[] = [
	100, 200, 500, 1000, 2000, 5000, 10000, 60000
];

/** 新しいクエリの初期値。名前は既存のクエリ名と衝突しない `query{n}`。 */
export function blankBridgeQuery(existing: readonly BridgeQueryFormState[]): BridgeQueryFormState {
	return {
		name: nextSequentialName(
			existing.map((q) => q.name),
			'query'
		),
		periodMs: 1000,
		sql: '',
		columnsText: ''
	};
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
		cpuType: 'R',
		database: '',
		username: '',
		password: '',
		clearPassword: false,
		staleTimeoutMs: '',
		bridgeEnabled: false,
		bridgeDbConnectionId: '',
		bridgeQueries: []
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
		cpuType: c.cpuType,
		database: c.database ?? '',
		username: c.username ?? '',
		password: '',
		clearPassword: false,
		...pushFormFromSettings(c.protocol === PUSH_PROTOCOL ? c.settings : null)
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
	if (form.protocol === PUSH_PROTOCOL) {
		// R2a-2e: push は接続先を持たない（サーバーが host "" / port 0 / 既定へ
		// 正規化する、§3.1）。`settings` は常に送る（update の省略は「保存済みの
		// 値を保つ」なので、フォームでブリッジを外したことが伝わるように）。
		return {
			name: form.name,
			protocol: form.protocol,
			host: '',
			port: 0,
			unitId: 1,
			enabled: form.enabled,
			simulation: false,
			wordOrder: form.wordOrder,
			cpuType: form.cpuType,
			settings: pushSettingsFromForm(form)
		};
	}
	const base: PlcConnectionInput = {
		name: form.name,
		protocol: form.protocol,
		host: form.host,
		port: Number(form.port),
		unitId: Number(form.unitId),
		enabled: form.enabled,
		simulation: form.simulation,
		wordOrder: form.wordOrder,
		cpuType: form.cpuType
	};
	if (form.protocol !== POSTGRES_PROTOCOL) return base;
	return {
		...base,
		database: form.database,
		username: form.username,
		password: passwordForSubmit(form)
	};
}

// --- R2a-2e: 取り込み口 `push` の設定（docs/banto-hub-ingest-design.md §3.2・§7.1） ---

/** {@link parseBridgeColumns} の結果。`error` は最初に見つけた行の誤り（無ければ `null`）。 */
export interface ParsedBridgeColumns {
	columns: PushBridgeColumnWire[];
	error: string | null;
}

/**
 * ブリッジのクエリの「列とキーの対応」の欄（1 行に 1 つの `列名=キー`）を読む。
 * 空行は飛ばす。列名とキーは前後の空白を除く（タグのキーは前後の空白を除いて
 * 保存されるので、§3.2 の「key の前後に空白」の拒否に当てない）。規則は
 * サーバー（`banto_tags::push` の `parse_column`）と同じ: 列名は結果列名
 * （英字か `_` で始まる英数字と `_`、63 文字まで）、キーは 1〜256 文字・制御
 * 文字なし。
 */
export function parseBridgeColumns(text: string): ParsedBridgeColumns {
	const columns: PushBridgeColumnWire[] = [];
	const lines = text.split(/\r?\n/);
	for (let i = 0; i < lines.length; i++) {
		const line = lines[i].trim();
		if (line === '') continue;
		const at = `${i + 1} 行目`;
		const eq = line.indexOf('=');
		if (eq < 0) return { columns, error: `${at}: 「列名=キー」の形で書いてください` };
		const column = line.slice(0, eq).trim();
		const key = line.slice(eq + 1).trim();
		if (
			Array.from(column).length > MAX_DB_COLUMN_NAME_LEN ||
			!DB_COLUMN_IDENTIFIER_RE.test(column)
		) {
			return {
				columns,
				error: `${at}: 列名は結果列名（英字またはアンダースコアで始まる英数字/アンダースコア、${MAX_DB_COLUMN_NAME_LEN} 文字まで）で書いてください`
			};
		}
		if (key === '') return { columns, error: `${at}: キーを書いてください` };
		if (Array.from(key).length > MAX_PUSH_KEY_LEN) {
			return { columns, error: `${at}: キーは ${MAX_PUSH_KEY_LEN} 文字までです` };
		}
		// eslint-disable-next-line no-control-regex
		if (/[\u0000-\u001f\u007f-\u009f]/.test(key)) {
			return { columns, error: `${at}: キーに制御文字は使えません` };
		}
		columns.push({ column, key });
	}
	return { columns, error: null };
}

/** フォームの push の欄から `settings`（§3.2）を組み立てる。 */
export function pushSettingsFromForm(form: PlcConnectionFormState): PushSettingsWire {
	const settings: PushSettingsWire = {};
	const stale = String(form.staleTimeoutMs).trim();
	if (stale !== '') settings.stale_timeout_ms = Number(stale);
	if (form.bridgeEnabled) {
		settings.bridge = {
			kind: 'db',
			db_connection_id: Number(form.bridgeDbConnectionId),
			queries: form.bridgeQueries.map((query) => ({
				name: query.name.trim(),
				period_ms: query.periodMs,
				sql: query.sql,
				columns: parseBridgeColumns(query.columnsText).columns
			}))
		};
	}
	return settings;
}

/** 保存済みの `settings` をフォームの push の欄へ（{@link pushSettingsFromForm} の逆）。 */
export function pushFormFromSettings(
	settings: PushSettingsWire | null | undefined
): Pick<
	PlcConnectionFormState,
	'staleTimeoutMs' | 'bridgeEnabled' | 'bridgeDbConnectionId' | 'bridgeQueries'
> {
	const bridge = settings?.bridge;
	return {
		staleTimeoutMs:
			settings?.stale_timeout_ms === undefined ? '' : String(settings.stale_timeout_ms),
		bridgeEnabled: bridge !== undefined,
		bridgeDbConnectionId: bridge ? String(bridge.db_connection_id) : '',
		bridgeQueries: (bridge?.queries ?? []).map((query) => ({
			name: query.name,
			periodMs: query.period_ms,
			sql: query.sql,
			columnsText: query.columns.map((c) => `${c.column}=${c.key}`).join('\n')
		}))
	};
}

/**
 * ブリッジの区画の送る量の見積もり（§7.1、`bridgeLoad.ts`）。ブリッジを持たない・
 * push でないなら `null`。列の欄が読めないクエリは読めた行までで数える（誤りは
 * {@link validatePushFields} が別に出す）。
 */
export function pushBridgeLoad(
	form: Pick<PlcConnectionFormState, 'protocol' | 'bridgeEnabled' | 'bridgeQueries'>
): BridgeLoad | null {
	if (form.protocol !== PUSH_PROTOCOL || !form.bridgeEnabled) return null;
	return bridgeLoad(
		form.bridgeQueries.map((query) => ({
			periodMs: query.periodMs,
			keys: parseBridgeColumns(query.columnsText).columns.map((c) => c.key)
		}))
	);
}

/** `bridgeQueries[i].<項目>` のエラーのキー（フォームの `errors` の添字）。 */
export function bridgeQueryErrorKey(index: number, field: 'name' | 'sql' | 'columns'): string {
	return `bridgeQueries[${index}].${field}`;
}

/**
 * 保存前の検証（push のときだけ。他プロトコルは空）。サーバー
 * （`validate_push_settings`）の規則のうち、画面で先に分かるものを写す:
 * `stale_timeout_ms` の範囲、ブリッジの読む接続の選択、クエリの名前（必須・
 * 64 文字・一意）と SQL（必須）、列の欄の形、キーの区画全体での一意、
 * **§7.1 の量の見積もり**（上限の 8 割を超えたら `bridge` に、サーバーと同じ
 * 文言で）。SQL の中身（SELECT / WITH・単文）はサーバーに任せる。
 */
export function validatePushFields(form: PlcConnectionFormState): Record<string, string> {
	const errors: Record<string, string> = {};
	if (form.protocol !== PUSH_PROTOCOL) return errors;

	const stale = String(form.staleTimeoutMs).trim();
	if (stale !== '') {
		const ms = Number(stale);
		if (!Number.isInteger(ms) || ms < MIN_STALE_TIMEOUT_MS || ms > MAX_STALE_TIMEOUT_MS) {
			errors.staleTimeoutMs = `${MIN_STALE_TIMEOUT_MS}〜${MAX_STALE_TIMEOUT_MS} の整数（ms）で入力してください`;
		}
	}
	if (!form.bridgeEnabled) return errors;

	if (form.bridgeDbConnectionId === '') {
		errors.bridgeDbConnectionId = 'ブリッジが読む PostgreSQL 接続を選んでください';
	}
	const names = new Set<string>();
	const keys = new Set<string>();
	form.bridgeQueries.forEach((query, index) => {
		const name = query.name.trim();
		if (name === '') {
			errors[bridgeQueryErrorKey(index, 'name')] = '必須項目です';
		} else if (Array.from(name).length > MAX_BRIDGE_QUERY_NAME_LEN) {
			errors[bridgeQueryErrorKey(index, 'name')] =
				`${MAX_BRIDGE_QUERY_NAME_LEN}文字以内で入力してください`;
		} else if (names.has(name)) {
			errors[bridgeQueryErrorKey(index, 'name')] = `クエリの名前 ${name} が重複しています`;
		}
		names.add(name);
		if (query.sql.trim() === '') errors[bridgeQueryErrorKey(index, 'sql')] = '必須項目です';
		const parsed = parseBridgeColumns(query.columnsText);
		if (parsed.error !== null) {
			errors[bridgeQueryErrorKey(index, 'columns')] = parsed.error;
			return;
		}
		for (const column of parsed.columns) {
			if (keys.has(column.key)) {
				errors[bridgeQueryErrorKey(index, 'columns')] =
					`キー ${column.key} が区画の中で重複しています`;
				return;
			}
			keys.add(column.key);
		}
	});
	const load = pushBridgeLoad(form);
	if (load && load.message !== null) errors.bridge = load.message;
	return errors;
}

/**
 * サーバーの項目エラー（`settings.*` の field）をフォームの `errors` の添字へ写す。
 * `settings.stale_timeout_ms` → `staleTimeoutMs`、`settings.bridge.db_connection_id`
 * → `bridgeDbConnectionId`、`settings.bridge.queries[i].name|sql` → その欄、
 * `settings.bridge.queries[i].columns…`・`.period_ms` → 列の欄、それ以外の
 * `settings…`（大きさ・見積もり・未知の項目）→ `bridge`。他の field はそのまま。
 */
export function mapPushFieldErrors(fieldErrors: Record<string, string>): Record<string, string> {
	const mapped: Record<string, string> = {};
	const put = (key: string, message: string) => {
		mapped[key] = mapped[key] ? `${mapped[key]} / ${message}` : message;
	};
	for (const [field, message] of Object.entries(fieldErrors)) {
		if (field === 'settings.stale_timeout_ms') {
			put('staleTimeoutMs', message);
			continue;
		}
		if (field === 'settings.bridge.db_connection_id') {
			put('bridgeDbConnectionId', message);
			continue;
		}
		const query = /^settings\.bridge\.queries\[(\d+)\](?:\.(\w+))?/.exec(field);
		if (query) {
			const index = Number(query[1]);
			const sub = query[2];
			const target = sub === 'name' || sub === 'sql' ? sub : 'columns';
			put(bridgeQueryErrorKey(index, target), message);
			continue;
		}
		if (field === 'settings' || field.startsWith('settings.')) {
			put('bridge', message);
			continue;
		}
		put(field, message);
	}
	return mapped;
}

/** 送り手向けの URL とスコープ（§5・§6。接続は id で指す、Q9 = (b)）。 */
export interface IngestEndpoints {
	/** REST の一括送信 `POST /api/v1/ingest/{id}/values`。 */
	restUrl: string;
	/** WebSocket `GET /api/v1/ingest/{id}/stream`。 */
	wsUrl: string;
	/** 送り手の API キーに要るスコープ `ingest:#{id}`。 */
	scope: string;
}

/**
 * `origin`（`window.location.origin`。管理画面を開いている Hub）と接続 id から、
 * 送り手向けの URL とスコープを組み立てる。WS は `http` → `ws`・`https` → `wss`。
 */
export function ingestEndpoints(origin: string, connectionId: number): IngestEndpoints {
	const base = origin.replace(/\/+$/, '');
	return {
		restUrl: `${base}/api/v1/ingest/${connectionId}/values`,
		wsUrl: `${base.replace(/^http/, 'ws')}/api/v1/ingest/${connectionId}/stream`,
		scope: `ingest:#${connectionId}`
	};
}

/**
 * 保存成功時の「保存済みの接続」の記録。`base` は保存時に親から渡されていた
 * `connection` prop（参照）、`saved` は更新 API が返した正規化済みの接続。
 */
export interface SavedConnectionRecord {
	base: PlcConnection;
	saved: PlcConnection;
}

/**
 * push 受信の表示（送り手 URL・スコープ・受信状態）と 3 秒ポーリングの基準に
 * する「保存済みの接続」を決める。親は保存後に一覧を取り直すだけで
 * `connection` prop を差し替えないため、プロトコル変更を保存した直後は prop が
 * 古いままになる。`record.base` が今の prop と同じ参照なら保存済みの `saved` を
 * 優先し、別の接続を開き直した（prop が変わった）ときは prop に戻す。
 * 未保存のフォーム（`form.protocol`）は決して基準にしない（保存前の切替に
 * 反応させない）。
 */
export function resolveSavedConnection(
	connection: PlcConnection | null,
	record: SavedConnectionRecord | null
): PlcConnection | null {
	if (connection && record && record.base === connection) return record.saved;
	return connection;
}

/** 保存済みの接続が push か（ingest ブロック表示・ポーリングの要否）。 */
export function isSavedPushConnection(
	connection: PlcConnection | null,
	record: SavedConnectionRecord | null
): boolean {
	return resolveSavedConnection(connection, record)?.protocol === PUSH_PROTOCOL;
}

/**
 * R2a-2e（§3.2「`postgres` 接続の削除は、`push` の `settings` が参照していれば
 * 断る」）: 接続 `connectionId` を DB ブリッジの読み取り先にしている push 接続
 * （画面が読み込み済みの一覧から引く）。削除の前に画面で先に止めるために使う
 * （最終の判定はサーバー - `banto_tags` の削除がどの入口でも断る）。
 */
export function pushBridgesReferencing(
	connections: readonly Pick<PlcConnection, 'id' | 'name' | 'protocol' | 'settings'>[],
	connectionId: number
): string[] {
	return connections
		.filter(
			(c) => c.protocol === PUSH_PROTOCOL && c.settings?.bridge?.db_connection_id === connectionId
		)
		.map((c) => c.name);
}

/** {@link pushBridgesReferencing} が空でないときの削除の拒否の文言。 */
export function formatBridgeReferenceDeleteRefusal(
	connectionName: string,
	bridgeNames: readonly string[]
): string {
	return `${connectionName} は push 接続（${bridgeNames.join('、')}）の DB ブリッジが読んでいるため削除できません。先にその push 接続のブリッジの設定を外すか、別の接続に変えてください。`;
}
