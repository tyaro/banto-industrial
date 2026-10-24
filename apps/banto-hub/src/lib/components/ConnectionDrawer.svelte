<script lang="ts">
	/**
	 * T18-6a（TAG-UX-7/TAG-UX-8、2026-08-27 オーナー決定「PLC接続の作成／
	 * 再設定を Drawer に寄せる」）: PLC接続の作成・再設定を担う共通部品。
	 * `Drawer.svelte`（汎用スライドオーバー、T13-1）を内包した自己完結
	 * コンポーネントにしてあるので、呼び出し側は `Drawer` を別途
	 * インスタンス化せず `<ConnectionDrawer open={...} connection={...} .../>`
	 * を並べるだけでよい。`plc-connections/+page.svelte` 単独ページと、
	 * 将来のタグツリー右クリック（T18-6d、別スライスが配線予定）の双方
	 * から使うことを狙って `src/lib/components/` に置く（`tags/+page.svelte`
	 * は既に4231行あり、これ以上ページ内実装を足さないための切り出し）。
	 *
	 * **新規作成 = ウィザード（3ステップ）、再設定 = 単一フォーム**
	 * （実装指示のとおり。`connection` prop が `null` かどうかで分岐する）。
	 * ステップは既存フィールドを自然に分けただけで、フィールド・検証は
	 * 旧 `plc-connections/+page.svelte` のページ内実装（BantoGrid一覧+
	 * 編集パネル構成、T9/T12/P3-bの成果）から1つも落としていない。
	 * 1. 識別: 名前（TAG-UX-8 の連番プリフィル対象）
	 * 2. プロトコルと接続先: プロトコル（`GET /api/drivers` のドライバ + ホストの
	 *    接続 postgres/push）と、そのドライバのスキーマから組み立てた欄
	 *    （R2b-6c、`DriverSettingsForm.svelte`。ホスト/ポート/ユニットID/ワード順/
	 *    CPU 種別などはスキーマが決める）/有効/シミュレーション
	 * 3. 接続テスト・確認: 入力内容の確認表示 + 接続テスト + 「作成」
	 *
	 * **T19 S1-b（UX-31、2026-09-02 オーナー決定「作成＝中央モーダル、
	 * 編集＝右ペイン」）: 作成ウィザードは `Modal.svelte`（中央）、再設定
	 * フォームと閲覧専用モードは `Drawer.svelte`（右ペイン）で描画する**
	 * （下のマークアップの `{#if isCreate}` で提示先ごと分岐 - `isCreate`
	 * は `connection` prop の null 性から決まり、Drawer を開いている間に
	 * 値が変わることはない前提のため、`open` だけを `isCreate` で振り分ける
	 * 単純な分岐で安全）。`Drawer.svelte` が持つフォーカストラップ・
	 * 二重発火防止・オーバーレイの仕組みは `Modal.svelte` も同じ契約
	 * （`onRequestClose`）で提供するため、呼び出し側（本ファイル）の
	 * ロジックはどちらの提示先でも変更していない。
	 *
	 * 純関数部分（連番採番・既定ポート・フォーム⇄API入力変換）は
	 * `#lib/banto/plcConnectionForm.ts` へ切り出し済み（そちらでユニット
	 * テスト済み）。
	 *
	 * **連番プリフィルは pending queue も見る**（実機で再現した不具合の修正1、
	 * 2026-08-31 オーナー報告 - `CollectionGroupDrawer.svelte` と同じ不具合が
	 * PLC接続側にもあった）: 収集稼働中の作成は 202 でキューに入るだけで
	 * DB（`existingNames`）には現れないため、既存レコードだけを見る連番採番
	 * では稼働中に Drawer を複数回開くたびに同じ名前が提案され、後から
	 * 一括適用すると名前の一意制約で全滅する。開いた直後は従来どおり
	 * `existingNames` だけで即座に仮の名前を出しつつ、裏で
	 * `listPendingChanges()`（admin 限定 API）を取得して pending 内の
	 * 未適用の `plc_connections.create` 分（`pendingCreateNames.ts`）も
	 * 候補に加え直す。ユーザーが名前欄を編集する前に取得が終われば
	 * 差し替える（`provisionalName`）。**pending の取得に失敗（権限不足含む）
	 * しても既存レコードだけでの採番のまま続行する**（プリフィルは利便性
	 * 機能であり、これで作成自体を止めない）。詳細は
	 * `CollectionGroupDrawer.svelte` の同名コメント参照。
	 *
	 * **202 (QueuedWhileRunningError, 収集稼働中のキュー投入) の扱い**
	 * （実装指示5）: 失敗ではなく案内として `toastStore.push('info', ...)`
	 * を使う（汎用エラーの `'error'` と区別する）。`tags/+page.svelte` は
	 * 現状これも汎用エラートーストへ委ねているだけだが、メッセージ自体は
	 * バックエンドの案内文「収集中のため変更を未適用キューに保存しました。」
	 * （`apps/banto-hub/core/src/rest.rs::queue_pending_registry_change`）
	 * そのままなので、ここでは文言はそちらと揃えたまま toast の種別だけ
	 * 'info' に分けている。Drawer は閉じない（tags 側の他の書き込みハンドラ
	 * と同じく、キュー投入時はフォームを保持したまま案内するだけ）。
	 *
	 * **2026-09-15 追補（誤爆防止 - オーナー報告「設定中に操作ミスで閉じて
	 * しまい最初からやり直しになる」）**: この Drawer/Modal は元々
	 * `onRequestClose` が `!isBusy()` のみで、**未保存確認そのものが無く**
	 * Esc・オーバーレイクリックで確認なしに即閉じていた
	 * （`tags/+page.svelte` のタグ編集 Drawer は `confirmDiscardIfNeeded`
	 * で確認していたのに、ここには揃っていなかった）。`tags/+page.svelte`
	 * と同じ流儀で `baseline`（Drawer/Modal を開いた時点のフォーム
	 * スナップショット）を持ち、`isFormDirty(baseline, form)` を
	 * `Drawer.svelte`/`Modal.svelte` の `dirty` prop へ渡す - `dirty` の間は
	 * Esc・オーバーレイクリックでは閉じない（`@banto/ui` の `isCloseAllowed`）。
	 * `onRequestClose`（`×` 経由）も `!isBusy()` だけでなく `dirty` なら
	 * `window.confirm` で確認するよう改めた（`confirmDiscardIfNeeded` と
	 * 同じ文言・同じ順序）。3ステップの作成ウィザードでも `baseline` は
	 * 開いた時点のまま変わらないため、ステップを進めて入力するだけで自然に
	 * `dirty` になる。
	 *
	 * **R2a-2e（docs/banto-hub-ingest-design.md §3.1・§3.2・§7.1・§9）: 取り込み口
	 * `push`**。プロトコルに `push` を選ぶと、接続先（ホスト・ポート等）の代わりに
	 * `stale_timeout_ms` とブリッジの区画（読む `postgres` 接続・クエリ・列とキーの
	 * 対応）を出し、ブリッジの送る量の見積もり（`bridgeLoad.ts`、§7.1）をその場で
	 * 出す。上限の 8 割を超えたら赤く示し、保存の前に断る（`validatePushFields`）。
	 * 再設定のドロワーでは、送り手向けの URL（`/api/v1/ingest/{id}/…`）とスコープ
	 * （`ingest:#{id}`）、受信の状態（`GET /api/status` の `ingest` 節、3 秒ごと）を
	 * 出す。状態は資格情報と値を含まない（サーバーが出さない）。送り手の `status` の
	 * 文は画面でも 1 KiB に切り詰める（`ingestStatusView.ts`）。ブリッジの区画は
	 * 入れ子の配列なので、`baseline` は `$state.snapshot` で取る（`{ ...form }` の
	 * 浅い複製では入れ子が `form` と共有されて `dirty` にならない）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { Drawer, Modal } from '@banto/ui';
	import { toastStore } from '#lib/toast.svelte.js';
	import { isAdmin } from '#lib/permissions.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { listPendingChanges, type PendingChange } from '#lib/banto/pendingChangesAdmin.js';
	import { pendingCreateNames } from '#lib/banto/pendingCreateNames.js';
	import { isFormDirty } from '#lib/banto/formDirty.js';
	import { passwordStatusView } from '#lib/banto/secretStorageView.js';
	import {
		createPlcConnection,
		deletePlcConnection,
		PUSH_PROTOCOL,
		isQueuedWhileRunningError,
		listDrivers,
		testPlcConnection,
		testSavedPlcConnection,
		updatePlcConnection,
		POSTGRES_PROTOCOL,
		type DbConnectionTestOutcome,
		type DriverInfo,
		type PlcConnection,
		type PlcConnectionTestResult
	} from '#lib/banto/tagRegistryAdmin.js';
	import DriverSettingsForm from '#lib/components/DriverSettingsForm.svelte';
	import {
		REQUIRED_MESSAGE,
		mapSettingsFieldErrors,
		summarizeDriverValues,
		validateDriverValues
	} from '#lib/banto/driverSettingsForm.js';
	import {
		protocolOptions,
		driverFor,
		driverStateAfterProtocolChange,
		DEFAULT_POSTGRES_PORT,
		BRIDGE_PERIOD_OPTIONS,
		DEFAULT_STALE_TIMEOUT_MS,
		blankBridgeQuery,
		blankConnectionForm,
		bridgeQueryErrorKey,
		formatBridgeReferenceDeleteRefusal,
		ingestEndpoints,
		isSavedPushConnection,
		resolveSavedConnection,
		type SavedConnectionRecord,
		mapPushFieldErrors,
		pushBridgeLoad,
		pushBridgesReferencing,
		validatePushFields,
		connectionToForm,
		formToConnectionInput,
		driverFieldsOnLoad,
		nextConnectionName,
		validatePostgresFields,
		type PlcConnectionFormState
	} from '#lib/banto/plcConnectionForm.js';
	import {
		countConnectionCascadeImpact,
		formatConnectionDeleteConfirmMessage
	} from '#lib/banto/registryCascadeImpact.js';
	import type { CollectionGroup, Tag } from '#lib/banto/tagRegistryAdmin.js';
	import {
		REGISTRATION_ITEMS_PER_SEC,
		REGISTRATION_MESSAGES_PER_SEC
	} from '#lib/banto/bridgeLoad.js';
	import { getHubStatus, type IngestStatusEntry } from '#lib/banto/hubStatus.js';
	import { createSerialPoller } from '#lib/banto/serialPoller.js';
	import {
		describeIngestSender,
		formatIngestTime,
		ingestLinkClass,
		ingestLinkLabel,
		truncateSenderStatus
	} from '#lib/banto/ingestStatusView.js';

	/** `pendingChangesAdmin.ts::PendingChange.source` - `rest.rs::plc_connections_create` が `queue_pending_registry_change` に渡す文字列と一致させる。 */
	const PENDING_SOURCE = 'plc_connections.create';

	interface Props {
		open: boolean;
		/** `null` なら新規作成（ウィザード）。非 `null` ならその接続の再設定（単一フォーム）。 */
		connection: PlcConnection | null;
		/** TAG-UX-8 の連番プリフィルに使う、既存の接続名一覧（新規作成時のみ参照）。 */
		existingNames: string[];
		/**
		 * T19 S2-b（UX-38、docs/banto-hub-t19-design.md §3.4）: 削除確認
		 * ダイアログで「消える定義の件数」を出すために必要な、ページが既に
		 * 読み込み済みの収集グループ・タグの一覧（新規作成時は参照しない）。
		 * `registryCascadeImpact.ts::countConnectionCascadeImpact` へそのまま
		 * 渡す。
		 */
		groups: CollectionGroup[];
		tags: Tag[];
		/**
		 * R2a-2e: ページが読み込み済みの接続の一覧（push のブリッジが読む `postgres`
		 * 接続の選択肢と、削除の前の参照の確認に使う）。省略時は空（選択肢なし）。
		 */
		connections?: PlcConnection[];
		/**
		 * T18-6d（タグツリー右クリック「接続を削除」からの起動）: `true` かつ
		 * `connection` が非 `null`（再設定モード）で Drawer を開いた直後、
		 * フォーム初期化に続けて既存の `handleDelete` をそのまま1回だけ呼ぶ -
		 * 確認ダイアログ（`window.confirm`）・削除影響エラー（収集グループが
		 * 参照している場合の Validation エラー）の扱いはすべて `handleDelete`
		 * の実装をそのまま流用し、ここでは独自の削除処理を持たない（実装指示の
		 * 制約）。確認をキャンセルした場合はこの Drawer に留まり、そのまま
		 * 再設定フォームとして使い続けられる。既定 `false`（単独ページからの
		 * 通常の再設定オープンでは何も起きない）。
		 */
		requestDelete?: boolean;
		/**
		 * T19 S1-a（docs/banto-hub-t19-design.md §7.1「viewer ロールからの
		 * 接続・グループ詳細の閲覧」）: `true` なら閲覧専用モードで開く -
		 * 入力はすべて `disabled`、接続テスト・保存・削除のボタンは出さない。
		 * viewer ロールはツリーの右クリックから編集 Drawer 自体を開けない
		 * （`tags/+page.svelte` が `canWrite` で `oncontextmenu` を丸ごと
		 * 落としている）ため、旧 `plc-connections` 画面が持っていた「全ロール
		 * 閲覧可・書き込みのみ制限」を、新規画面を作らずこの Drawer 自身に
		 * 持たせる形で再現する。常に既存の接続（`connection` 非 `null`）と
		 * 組み合わせて使う想定（新規作成フォームを閲覧専用で開く意味は無い）。
		 * 既定 `false` - 書き込み権限がある利用者の挙動は一切変えない。
		 */
		readOnly?: boolean;
		onClose: () => void;
		/** 作成/更新が成功した直後に呼ばれる（202キュー投入時は呼ばれない — まだ確定していないため）。 */
		onSaved: (conn: PlcConnection) => void;
		/**
		 * 削除が成功した直後に呼ばれる。**Promise を返せば完了を待ってから閉じる**
		 * （#381 レビュー対応16回目）: 呼び出し側の再読込（`reload()`）を待たずに
		 * 閉じると、`Drawer`/`Modal` のフォーカス戻しが「消える予定の行/ノード」へ
		 * 戻してしまい、直後に再読込がそれを消してフォーカスが `<body>` に落ちる。
		 */
		onDeleted: (id: number) => void | Promise<void>;
		/**
		 * #381 レビュー対応11回目: 閉じたときのフォーカスの戻し先の代替
		 * （`Drawer.svelte`/`Modal.svelte` の同名 prop へそのまま渡すだけ）。
		 * 右クリックメニューから開かれた場合、開いた元のメニュー項目は閉じる
		 * ころには DOM に居ないため、呼び出し側が代わりの要素を指定できる。
		 */
		focusFallback?: () => HTMLElement | null | undefined;
	}

	let {
		open,
		connection,
		existingNames,
		groups,
		tags,
		connections = [],
		requestDelete = false,
		readOnly = false,
		onClose,
		onSaved,
		onDeleted,
		focusFallback
	}: Props = $props();

	const isCreate = $derived(connection === null);
	const drawerTitle = $derived(
		isCreate ? '新規作成' : readOnly ? `${connection?.name} の詳細` : `${connection?.name} を編集`
	);

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	/** 422 の項目エラー（`field` → 文言）。検証エラーでなければ `null`。 */
	function fieldErrorMap(err: unknown): Record<string, string> | null {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const map: Record<string, string> = {};
			for (const fe of err.body.field_errors) map[fe.field] = fe.message;
			return map;
		}
		return null;
	}

	/** ホストの接続 postgres の `settings` の欄（サーバーの項目エラーを同名のフォームの欄へ写す）。 */
	const POSTGRES_SETTING_KEYS = ['host', 'port', 'database', 'username', 'password'];

	/**
	 * サーバーの項目エラーを画面へ写す: push は `settings.*` を push の欄へ、ドライバは
	 * スキーマの欄の key へ（`driverErrors`）、postgres は同名の欄へ。写せないもの（`name` 等）は
	 * そのまま `errors`。項目エラーだったら `true`。
	 */
	function applyFieldErrors(err: unknown): boolean {
		const map = fieldErrorMap(err);
		if (!map) return false;
		if (form.protocol === PUSH_PROTOCOL) {
			errors = mapPushFieldErrors(map);
			driverErrors = {};
		} else if (driver) {
			const mapped = mapSettingsFieldErrors(
				driver.settingsSchema.fields.map((f) => f.key),
				map
			);
			errors = mapped.other;
			driverErrors = mapped.fields;
		} else {
			const mapped = mapSettingsFieldErrors(POSTGRES_SETTING_KEYS, map);
			errors = { ...mapped.other, ...mapped.fields };
			driverErrors = {};
		}
		return true;
	}

	/**
	 * T12 (docs/ux-plan.md §4): 接続テストの実行状態。旧ページ実装から無改変
	 * で移設（作成・再設定それぞれ独立した `TestState` を持つ）。
	 */
	interface TestState {
		testing: boolean;
		result: PlcConnectionTestResult | null;
	}
	function blankTestState(): TestState {
		return { testing: false, result: null };
	}

	/**
	 * S1（docs/banto-hub-external-db-design.md §4.6）: 保存済み postgres
	 * 接続専用の接続テスト状態。`TestState`（T12、PLC の保存前テスト）とは
	 * 別に持つ - 対象エンドポイントも応答形も違う
	 * （`testSavedPlcConnection`/{@link DbConnectionTestOutcome}）ため。
	 */
	interface DbTestState {
		testing: boolean;
		result: DbConnectionTestOutcome | null;
	}
	function blankDbTestState(): DbTestState {
		return { testing: false, result: null };
	}

	/**
	 * R2b-6c: `GET /api/drivers`（機器のドライバとそのスキーマ）。開いたときに一度だけ読む。
	 * 読めなかったときは空の一覧に潰さず、エラーを出して再取得できるようにする
	 * （ホストの接続 postgres/push は一覧が無くても編集できる）。
	 */
	let drivers: DriverInfo[] = $state.raw([]);
	let driversStatus: 'idle' | 'loading' | 'ready' | 'error' = $state('idle');
	let driversError: string | null = $state(null);
	/** ドライバの欄のエラー（欄の key → 文言）。 */
	let driverErrors: Record<string, string> = $state({});

	let form: PlcConnectionFormState = $state(blankConnectionForm([]));
	const driver = $derived(driverFor(drivers, form.protocol));
	const protocolChoices = $derived(protocolOptions(drivers));
	/**
	 * 2026-09-15 追補: Drawer/Modal を開いた（または保存が成功した）時点の
	 * `form` スナップショット。`tags/+page.svelte::createBaseline` と同じ
	 * 流儀で通常の `let`（`$state` にしない） - `dirty` の `$derived` は
	 * `form`（`$state`）の変化で再評価されるため、`baseline` は毎回その
	 * 時点の最新値を読めれば十分。
	 */
	let baseline: PlcConnectionFormState = blankConnectionForm([]);
	/** `dirty` の間は Esc・オーバーレイクリックで閉じない（`Drawer.svelte`/`Modal.svelte` の `dirty` prop）。 */
	const dirty = $derived(isFormDirty(baseline, form));
	let errors: Record<string, string> = $state({});
	let saving = $state(false);
	let deleting = $state(false);
	let testState: TestState = $state(blankTestState());
	let dbTestState: DbTestState = $state(blankDbTestState());
	let step: 1 | 2 | 3 = $state(1);
	/** 直前のプロトコル（ドライバを替えたときの引き継ぎの元を引く）。 */
	let lastProtocol = '';

	async function loadDrivers(): Promise<void> {
		if (driversStatus === 'loading') return;
		driversStatus = 'loading';
		driversError = null;
		try {
			drivers = await listDrivers();
			driversStatus = 'ready';
			applyDriversToForm();
		} catch (err) {
			driversError = errorMessage(err);
			driversStatus = 'error';
		}
	}

	/**
	 * 一覧の読み込みが成功したとき、ドライバの欄（と空だったプロトコル）だけを補う。開いた時点で
	 * 一覧が無くても初期化は済んでおり、利用者が触った他の欄（名前など）は上書きしない。
	 * システム側の更新なので baseline もドライバの部分だけ揃えて dirty にしない
	 * （利用者が先に触った欄は dirty のまま残る）。
	 */
	function applyDriversToForm(): void {
		const patch = driverFieldsOnLoad(form, connection ? connection.protocol : null, drivers);
		if (!patch) return;
		if (patch.protocol !== undefined) {
			form.protocol = patch.protocol;
			lastProtocol = patch.protocol;
		}
		form.driver = patch.driver;
		baseline = {
			...baseline,
			...(patch.protocol !== undefined ? { protocol: patch.protocol } : {}),
			driver: $state.snapshot(form.driver)
		};
	}

	$effect(() => {
		if (open && driversStatus === 'idle') void loadDrivers();
	});

	/**
	 * Drawer を開いた対象（新規作成 or どの接続の再設定か）を表すキー。
	 * `open` が false→true になった時、または既に開いた状態のまま
	 * 別の接続（`connection.id` が変わる = 一覧で別の行を選び直した）へ
	 * 切り替わった時だけフォームを初期化し直す。保存成功後に親が
	 * `connections` を再取得して新しい `PlcConnection` オブジェクト
	 * （同じ id）を渡してきても、このキーは変わらないため未保存編集を
	 * 巻き戻さない（`handleSave` 側が保存直後に明示的に `form` を
	 * 正規化済みの値へ差し替える - tags/+page.svelte の `saveEdit` と同じ
	 * 方針）。
	 */
	let lastOpenKey: string | null = null;

	/**
	 * 修正1（実機で再現した不具合、2026-08-31 オーナー報告）: 新規作成フォームを
	 * 開いた直後に `nextConnectionName(existingNames)` だけで即座に割り当てた
	 * 「仮の」名前。pending queue 取得が完了した後、ユーザーがまだ名前欄を
	 * 編集していなければ（`form.name === provisionalName`）pending も
	 * 反映した名前へ差し替える。ユーザーが既に編集していれば上書きしない。
	 */
	let provisionalName: string | null = null;

	$effect(() => {
		if (!open) {
			lastOpenKey = null;
			return;
		}
		// 初期化は開いた時点で行う（ドライバの一覧は待たない。取得の完了は `applyDriversToForm` が
		// ドライバの欄だけ補う）。
		const key = connection ? `edit:${connection.id}` : 'create';
		if (key === lastOpenKey) return;
		lastOpenKey = key;

		if (connection) {
			form = connectionToForm(connection, drivers);
			provisionalName = null;
		} else {
			const blank = blankConnectionForm(drivers);
			const initialName = nextConnectionName(existingNames);
			blank.name = initialName;
			provisionalName = initialName;
			form = blank;
			void refinePendingNamePrefill(key);
		}
		baseline = $state.snapshot(form);
		errors = {};
		driverErrors = {};
		testState = blankTestState();
		dbTestState = blankDbTestState();
		step = 1;
		lastProtocol = form.protocol;

		// T18-6d: 「接続を削除」からの起動 - フォーム初期化直後に既存の
		// handleDelete を1回だけ呼ぶ（上の Props.requestDelete 参照）。
		// readOnly では呼び出し側が requestDelete を渡すことは無い想定だが、
		// 念のため二重に閲覧専用を守る。
		if (requestDelete && connection && !readOnly) {
			void handleDelete();
		}
	});

	/**
	 * pending queue（`GET /api/pending-changes`、admin 限定）を取得し、
	 * まだ適用されていない `plc_connections.create` の名前も連番プリフィル
	 * の衝突候補に加えて `form.name` を差し替える。上の `$effect` からの
	 * fire-and-forget 呼び出し専用（本体はモジュール doc comment の
	 * 「連番プリフィルは pending queue も見る」参照 - `CollectionGroupDrawer.
	 * svelte::refinePendingNamePrefill` と同じ実装）。
	 *
	 * - admin 以外は `/api/pending-changes` が 403（`RoleGuard` が
	 *   `denied` を監査ログに記録する）になるだけなので、そもそも叩かない
	 *   - editor での通常の作成操作のたびに監査ログを汚さないため。
	 * - 取得に失敗しても（ネットワークエラー等）既存レコードだけの採番の
	 *   まま続行する（プリフィルは利便性機能であり、これで作成自体は止めない）。
	 * - Drawer が既に閉じられた／別の対象で開き直された（`openKey` が
	 *   `lastOpenKey` と一致しない）場合は結果を捨てる。
	 */
	async function refinePendingNamePrefill(openKey: string): Promise<void> {
		if (!isAdmin(sessionStore.role)) return;
		let pending: PendingChange[];
		try {
			pending = await listPendingChanges();
		} catch {
			return;
		}
		if (lastOpenKey !== openKey) return;
		const pendingNames = pendingCreateNames(pending, PENDING_SOURCE);
		if (pendingNames.length === 0) return;
		const refined = nextConnectionName(existingNames, 'connection', pendingNames);
		if (form.name === provisionalName) {
			// baseline も一緒に差し替える - これはシステム側のプリフィル
			// 更新であってユーザーの未保存編集ではないため、dirty 扱いに
			// してはいけない（`tags/+page.svelte::openCreateDrawer` の
			// 「プリセットした値は createBaseline にも入れる」と同じ配慮）。
			form.name = refined;
			baseline = { ...baseline, name: refined };
		}
		provisionalName = refined;
	}

	/**
	 * プロトコルを替えたとき: ドライバ → ドライバは「同じキーで利用者が触った欄の値だけ引き継ぎ、
	 * 他は新しいスキーマの既定で埋め直す」、ホストの接続 → ドライバは既定で埋める
	 * （`driverStateAfterProtocolChange`）。
	 */
	function onProtocolChange(): void {
		form.driver = driverStateAfterProtocolChange(drivers, lastProtocol, form.protocol, form.driver);
		lastProtocol = form.protocol;
		driverErrors = {};
		errors = {};
		testState = blankTestState();
	}

	/** 送信前のフィールド → 該当ウィザードステップの対応（作成時のエラー誘導用）。 */
	const FIELD_STEP: Record<string, 1 | 2 | 3> = {
		name: 1,
		protocol: 2,
		host: 2,
		port: 2,
		// S1（docs/banto-hub-external-db-design.md §4.1）: postgres 接続の
		// 追加フィールドも「2. プロトコルと接続先」ステップに置く
		// （destinationFields スニペット内、下記マークアップ参照）。
		database: 2,
		username: 2,
		password: 2,
		// R2a-2e: push の欄も「2. プロトコルと接続先」（`bridgeQueries[i].*` は
		// 下の `stepForFieldErrors` が接頭辞で 2 へ寄せる）。
		staleTimeoutMs: 2,
		bridgeDbConnectionId: 2,
		bridge: 2
	};

	function stepForFieldErrors(fieldErrors: Record<string, string>): 1 | 2 | 3 | null {
		let target: 1 | 2 | 3 | null = null;
		for (const field of Object.keys(fieldErrors)) {
			const s = FIELD_STEP[field] ?? (field.startsWith('bridgeQueries[') ? 2 : undefined);
			if (s !== undefined && (target === null || s < target)) target = s;
		}
		return target;
	}

	/** 一覧の取得中は、ドライバの欄に頼る操作（次へ・保存・接続テスト・プロトコル選択）を止める。 */
	const driversBlocking = $derived.by(() => {
		const status: string = driversStatus;
		return (
			status === 'loading' && form.protocol !== POSTGRES_PROTOCOL && form.protocol !== PUSH_PROTOCOL
		);
	});

	const canAdvanceFromStep1 = $derived(form.name.trim() !== '');
	/**
	 * S1: postgres 接続は`host`に加えて`database`/`username`も必須
	 * （サーバー側`validate_plc_connection_input`と同じ要件 -
	 * `validatePostgresFields`の doc comment参照）。ここでは「次へ」ボタンの
	 * 活性化判定だけを軽く行い、実際のフィールドごとのエラー文言は
	 * 送信時（`handleCreate`/`handleSave`）の`validatePostgresFields`が出す。
	 */
	const canAdvanceFromStep2 = $derived(
		// R2a-2e: push は接続先を持たない（ホスト不要）。push の欄の誤りは送信時の
		// `validatePushFields` が出し、見積もりの超過は欄の下にその場で赤く出る。
		(form.protocol === PUSH_PROTOCOL && (pushBridgeLoad(form)?.withinLimit ?? true)) ||
			// R2b-6c: ドライバは必須の欄が埋まっていれば進める（範囲などの誤りは送信時に出す）。
			(driver !== undefined &&
				!Object.values(
					validateDriverValues(driver.settingsSchema.fields, form.driver.values)
				).includes(REQUIRED_MESSAGE)) ||
			(form.protocol === POSTGRES_PROTOCOL &&
				form.host.trim() !== '' &&
				form.database.trim() !== '' &&
				form.username.trim() !== '')
	);

	/** R2a-2e: ブリッジの送る量の見積もり（§7.1）。push でブリッジを持つときだけ。 */
	const bridgeLoadEstimate = $derived(pushBridgeLoad(form));

	/** R2a-2e: ブリッジが読める接続（`postgres` だけ。自分自身は出さない）。 */
	const postgresConnections = $derived(
		connections.filter((c) => c.protocol === POSTGRES_PROTOCOL && c.id !== connection?.id)
	);

	function addBridgeQuery(): void {
		form.bridgeQueries = [...form.bridgeQueries, blankBridgeQuery(form.bridgeQueries)];
	}

	function removeBridgeQuery(index: number): void {
		form.bridgeQueries = form.bridgeQueries.filter((_, i) => i !== index);
	}

	/**
	 * R2a-2e（§9）: 再設定のドロワーで、push 接続の受信の状態（`GET /api/status`
	 * の `ingest` 節）。開いている間だけ 3 秒ごとに取り直す。取得の失敗は状態を
	 * 空にせず、失敗した旨を別に出す（checklist §5「エラーを空に潰さない」）。
	 */
	const INGEST_STATUS_POLL_MS = 3000;
	let ingestStatus: IngestStatusEntry | null = $state(null);
	let ingestStatusLoaded = $state(false);
	let ingestStatusError: string | null = $state(null);

	/**
	 * 保存成功時に更新 API が返した接続（P2 レビュー対応）。親は一覧を取り直す
	 * だけで `connection` prop を差し替えないため、保存したプロトコルの変更
	 * （空の PLC 接続 → push など）を表示・ポーリングへ反映する基準にする。
	 * 未保存の `form.protocol` は基準にしない。
	 */
	let savedRecord: SavedConnectionRecord | null = $state.raw(null);
	const savedConnection = $derived(resolveSavedConnection(connection, savedRecord));
	const savedIsPush = $derived(isSavedPushConnection(connection, savedRecord));

	$effect(() => {
		const id = savedConnection?.id;
		if (!open || id === undefined || !savedIsPush) return;
		ingestStatus = null;
		ingestStatusLoaded = false;
		ingestStatusError = null;
		// 直列ポーラー（PR #760 の Codex レビュー P2）: 前の取得が決着してから次を
		// 予約し、閉じる・push でなくなる・破棄で進行中の要求を中断する。止めた後に
		// 決着した古い応答は状態へ書かない。
		const poller = createSerialPoller({
			intervalMs: INGEST_STATUS_POLL_MS,
			// 応答が来ない回は 10 秒で打ち切り、時間切れを表示して次を予約する。
			timeoutMs: 10_000,
			run: (signal) => getHubStatus(signal),
			onResult: (status) => {
				ingestStatus = status.ingest.find((entry) => entry.connection_id === id) ?? null;
				ingestStatusError = null;
			},
			onError: (err) => {
				ingestStatusError = errorMessage(err);
			},
			onSettled: () => {
				ingestStatusLoaded = true;
			}
		});
		return () => poller.stop();
	});

	/** 送り手向けの URL とスコープ（保存済みの push 接続だけ）。 */
	const endpoints = $derived(
		savedConnection && savedIsPush
			? ingestEndpoints(
					typeof window === 'undefined' ? '' : window.location.origin,
					savedConnection.id
				)
			: null
	);

	function goNext(): void {
		if (step === 1 && canAdvanceFromStep1) step = 2;
		else if (step === 2 && canAdvanceFromStep2) {
			// 範囲などの誤りは手順 3 へ進む前に欄の下へ出す（接続テストの理由が見えない状態を作らない）。
			if (driver) {
				const found = validateDriverValues(driver.settingsSchema.fields, form.driver.values);
				driverErrors = found;
				if (Object.keys(found).length > 0) return;
			}
			step = 3;
		}
	}

	function goBack(): void {
		if (step > 1) step = (step - 1) as 1 | 2 | 3;
	}

	/**
	 * `connectionId` は「保存済み接続の再設定フォームからのテスト」のときだけ
	 * `connection.id` を渡す（新規作成フォームは常に `undefined`）。
	 */
	async function runConnectionTest(): Promise<void> {
		if (testState.testing) return; // 多重クリック防止
		if (driver) {
			// 範囲などの誤りは送る前に欄の下へ出す（サーバーと同じ規則）。
			const found = validateDriverValues(driver.settingsSchema.fields, form.driver.values);
			driverErrors = found;
			if (Object.keys(found).length > 0) {
				// 手順 3（確認）にはドライバの欄が無い。保存時の validateBeforeSubmit と同じく手順 2 へ戻す。
				if (isCreate && step === 3) step = 2;
				return;
			}
		}
		testState.testing = true;
		testState.result = null;
		try {
			// フォームの現在値（未保存でもよい）をそのまま送る。秘密の欄の「変えない」はキーを
			// 送らず、`connectionId` の保存済みから補われる。
			const input = formToConnectionInput(form, drivers);
			testState.result = await testPlcConnection({
				protocol: input.protocol,
				simulation: input.simulation,
				settings: input.settings,
				connectionId: connection?.id
			});
		} catch (err) {
			// 401/403・CSRF拒否・ネットワークエラーなど（`ok: false` はここに来ない
			// 通常応答 — 上の try 内で result にそのまま入る）。
			toastStore.push('error', errorMessage(err));
		} finally {
			testState.testing = false;
		}
	}

	/**
	 * S1（docs/banto-hub-external-db-design.md §4.6）:
	 * `POST /api/plc-connections/{id}/test`（保存済み postgres 接続専用）を
	 * 呼ぶ。**未保存の接続では呼ばない** - マークアップ側で`connection`が
	 * `null`のときはボタン自体を disabled にし、「保存後にテストできます」
	 * のヒントを出す（実装指示3）。フォームの現在入力値ではなく、常に
	 * 保存済みの値（サーバー側で再取得）をテストする点が T12 の
	 * `runConnectionTest`（フォームの現在値をそのまま送る）と違う - 設計上
	 * postgres 接続はパスワードをフォームに再表示しないため、そもそも
	 * フォームの現在値だけではテストしようがない。
	 */
	async function runDbConnectionTest(): Promise<void> {
		if (!connection || dbTestState.testing) return; // 多重クリック防止・未保存ガード
		dbTestState.testing = true;
		dbTestState.result = null;
		try {
			dbTestState.result = await testSavedPlcConnection(connection.id);
		} catch (err) {
			// 401/403・CSRF拒否・ネットワークエラー・400（postgres以外）など
			// （`ok: false` はここに来ない通常応答 — 上の try 内で result に
			// そのまま入る）。
			toastStore.push('error', errorMessage(err));
		} finally {
			dbTestState.testing = false;
		}
	}

	/**
	 * S1: `validatePostgresFields`（サーバー側 postgres 必須ルールのミラー）
	 * を送信前に走らせる。エラーがあれば`errors`へ積んで対応ステップへ誘導
	 * し、`false`を返してAPI呼び出し自体を止める - サーバーの422を待たず
	 * クライアント側で弾く（実装指示2「クライアント側検証はサーバー側の
	 * ルールを反映する」）。
	 */
	function validateBeforeSubmit(): boolean {
		// R2a-2e: push の欄（範囲・ブリッジの形・§7.1 の見積もり）も送る前に確かめる。
		const fieldErrors = { ...validatePostgresFields(form), ...validatePushFields(form) };
		const driverFound = driver
			? validateDriverValues(driver.settingsSchema.fields, form.driver.values)
			: {};
		driverErrors = driverFound;
		if (Object.keys(fieldErrors).length === 0 && Object.keys(driverFound).length === 0) return true;
		errors = fieldErrors;
		const target = stepForFieldErrors(fieldErrors);
		if (target !== null) step = target;
		else if (Object.keys(driverFound).length > 0) step = 2;
		return false;
	}

	async function handleCreate(): Promise<void> {
		if (!validateBeforeSubmit()) return;
		saving = true;
		errors = {};
		driverErrors = {};
		try {
			const created = await createPlcConnection(formToConnectionInput(form, drivers));
			toastStore.push('success', '作成しました');
			onSaved(created);
			onClose();
		} catch (err) {
			if (isQueuedWhileRunningError(err)) {
				// 実装指示5: 失敗ではなく案内として扱う（Drawerは開いたまま）。
				toastStore.push('info', err.message);
				return;
			}
			if (applyFieldErrors(err)) {
				const target = stepForFieldErrors(errors);
				if (target !== null) step = target;
				else if (Object.keys(driverErrors).length > 0) step = 2;
			} else {
				toastStore.push('error', errorMessage(err));
			}
		} finally {
			saving = false;
		}
	}

	async function handleSave(): Promise<void> {
		if (!connection) return;
		if (!validateBeforeSubmit()) return;
		saving = true;
		errors = {};
		driverErrors = {};
		try {
			const updated = await updatePlcConnection(
				connection.id,
				formToConnectionInput(form, drivers)
			);
			toastStore.push('success', '更新しました');
			// 保存成功後はサーバーの正規化値を基準に取り直す（tags/+page.svelte
			// の saveEdit と同じ方針）。Drawer は閉じない。baseline も同じ値へ
			// 揃えることで、保存直後は dirty ではない状態に戻す。
			form = connectionToForm(updated, drivers);
			lastProtocol = form.protocol;
			baseline = $state.snapshot(form);
			savedRecord = { base: connection, saved: updated };
			onSaved(updated);
		} catch (err) {
			if (isQueuedWhileRunningError(err)) {
				toastStore.push('info', err.message);
				return;
			}
			if (!applyFieldErrors(err)) toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	/**
	 * T19 S2-b（UX-38）: 確認メッセージに「消える定義の件数」と「履歴は残る」
	 * を明示する（実装指示「何件消えるか分からないまま削除させない」）。
	 * バックエンドはこの接続を消すと配下の収集グループ・タグを全部まとめて
	 * 削除する（`cascade_delete_tx` - もう拒否しない）ため、影響が見えない
	 * と事故になる。
	 */
	async function handleDelete(): Promise<void> {
		if (!connection) return;
		// R2a-2e（§3.2）: push のブリッジが読んでいる postgres 接続は削除できない。
		// 読み込み済みの一覧で分かるなら確認ダイアログより前に止める（サーバーも
		// 同じ理由で断る - 一覧が古いときは下の catch に同じ趣旨の文言が来る）。
		const bridges = pushBridgesReferencing(connections, connection.id);
		if (bridges.length > 0) {
			toastStore.push('error', formatBridgeReferenceDeleteRefusal(connection.name, bridges));
			return;
		}
		const impact = countConnectionCascadeImpact(connection.id, groups, tags);
		if (!window.confirm(formatConnectionDeleteConfirmMessage(connection.name, impact))) return;
		deleting = true;
		try {
			await deletePlcConnection(connection.id);
			toastStore.push('success', '削除しました');
			// 再読込（`onDeleted`）の完了を待ってから閉じる - 上の prop の doc 参照。
			await onDeleted(connection.id);
			onClose();
		} catch (err) {
			if (isQueuedWhileRunningError(err)) {
				toastStore.push('info', err.message);
				return;
			}
			// 収集グループ・sink group・push のブリッジ（R2a-2e）が参照している
			// 場合はサービス層の分かりやすい Validation エラー（件数・名前入り）が
			// ここに来る。項目の文言があればそれを出す。
			toastStore.push('error', fieldErrorMap(err)?.id ?? errorMessage(err));
		} finally {
			deleting = false;
		}
	}

	function isBusy(): boolean {
		return saving || deleting || testState.testing || dbTestState.testing;
	}

	/**
	 * 2026-09-15 追補: 処理中は閉じさせない（従来どおり）。加えて、未保存の
	 * 変更（`dirty`）があれば `tags/+page.svelte::confirmDiscardIfNeeded` と
	 * 同じ文言・同じ順序で `window.confirm` による破棄確認を行う - 従来は
	 * この確認自体が無く、`×` を含むすべての経路で無確認に閉じていた。
	 */
	function onRequestClose(): boolean {
		if (isBusy()) return false;
		if (dirty && !window.confirm('変更を破棄しますか？')) return false;
		return true;
	}

	/** `dirty` のため Esc/オーバーレイクリックが弾かれたことを案内する。 */
	function notifyBlockedClose(): void {
		toastStore.push('info', '未保存の変更があります。閉じるには × を押してください。');
	}
</script>

{#snippet nameField()}
	<label class="field">
		名前
		<input type="text" id="connection-name" bind:value={form.name} disabled={readOnly} />
		{#if errors.name}<span class="err">{errors.name}</span>{/if}
	</label>
{/snippet}

{#snippet destinationFields()}
	<div class="form-grid">
		<label class="field">
			プロトコル
			<select
				bind:value={form.protocol}
				onchange={onProtocolChange}
				disabled={readOnly || driversBlocking}
			>
				{#each protocolChoices as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
				{#if form.protocol !== '' && !protocolChoices.some((o) => o.value === form.protocol)}
					<!-- 一覧に無いプロトコル（virtual など）は、そのまま見える形で残す。 -->
					<option value={form.protocol}>{form.protocol}</option>
				{/if}
			</select>
			{#if errors.protocol}<span class="err">{errors.protocol}</span>{/if}
		</label>
		{#if driversStatus === 'loading'}
			<p class="hint wide" data-testid="drivers-loading">ドライバの一覧を読み込み中…</p>
		{:else if driversStatus === 'error'}
			<!-- 一覧が読めないことを「ドライバ 0 件」に潰さず、理由と再取得を出す。 -->
			<div class="wide drivers-error" role="alert" data-testid="drivers-error">
				<span class="err">ドライバの一覧を読めませんでした: {driversError}</span>
				<button type="button" class="secondary" onclick={() => void loadDrivers()}>再取得</button>
			</div>
		{/if}
		{#if form.protocol === PUSH_PROTOCOL}
			{@render pushFields()}
		{:else if form.protocol === POSTGRES_PROTOCOL}
			<label class="field">
				ホスト
				<input type="text" bind:value={form.host} placeholder="192.168.1.10" disabled={readOnly} />
				{#if errors.host}<span class="err">{errors.host}</span>{/if}
			</label>
			<label class="field">
				ポート
				<input
					type="number"
					min="1"
					max="65535"
					bind:value={form.port}
					placeholder={String(DEFAULT_POSTGRES_PORT)}
					disabled={readOnly}
				/>
				<span class="hint">PostgreSQL の待受ポート（既定 5432）。</span>
				{#if errors.port}<span class="err">{errors.port}</span>{/if}
			</label>
			<!--
				S1（docs/banto-hub-external-db-design.md §4.1）: postgres 接続
				専用フィールド（`settings` の database / username / 秘密の password）。
			-->
			<label class="field">
				データベース
				<input type="text" bind:value={form.database} placeholder="appdb" disabled={readOnly} />
				{#if errors.database}<span class="err">{errors.database}</span>{/if}
			</label>
			<label class="field">
				ユーザー名
				<input
					type="text"
					bind:value={form.username}
					placeholder="appuser"
					disabled={readOnly}
					autocomplete="off"
				/>
				{#if errors.username}<span class="err">{errors.username}</span>{/if}
			</label>
			{#if !readOnly}
				{#if isCreate}
					<label class="field">
						パスワード
						<input type="password" bind:value={form.password} autocomplete="new-password" />
						<span class="hint">空欄のままでも作成できます（後から設定できます）。</span>
					</label>
				{:else}
					{@const passwordView = passwordStatusView(form.passwordState)}
					<div
						class="field password-status"
						class:unreadable={passwordView.unreadable}
						role={passwordView.unreadable ? 'alert' : undefined}
						data-testid="password-status"
					>
						{passwordView.text}
					</div>
					<label class="field">
						新しいパスワード（変更する場合のみ入力）
						<input
							type="password"
							bind:value={form.password}
							placeholder="変更しない"
							autocomplete="new-password"
							disabled={form.clearPassword}
						/>
					</label>
					<label class="field checkbox">
						<input type="checkbox" bind:checked={form.clearPassword} />
						パスワードを消去する
					</label>
				{/if}
				{#if errors.password}<span class="err">{errors.password}</span>{/if}
			{/if}
		{:else if driver}
			<!-- 機器のドライバの欄はスキーマから描く（R2b-6c、設計 §6.1）。 -->
			<DriverSettingsForm
				schema={driver.settingsSchema.fields}
				bind:value={form.driver}
				errors={driverErrors}
				{readOnly}
				idPrefix="conn-driver"
			/>
		{:else if driversStatus === 'ready' && form.protocol !== ''}
			<p class="hint wide">このプロトコルの設定欄はありません（{form.protocol}）。</p>
		{/if}
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} disabled={readOnly} />
			有効
		</label>
		{#if driver}
			<label class="field checkbox">
				<input type="checkbox" bind:checked={form.simulation} disabled={readOnly} />
				シミュレーションモード
			</label>
			<span class="hint sim-hint">
				実PLCの代わりに内蔵シミュレータに接続します（開発・検証用）。本番運用では有効にしないでください。
			</span>
		{/if}
	</div>
{/snippet}

{#snippet pushFields()}
	<!--
		R2a-2e（docs/banto-hub-ingest-design.md §3.2・§4.3・§7.1）: push 接続は接続先を
		持たず（サーバーが host / port を正規化する）、受信のタイムアウトと DB ブリッジの
		区画だけを持つ。ブリッジの送る量の見積もりは入力のたびに出し、上限の 8 割を
		超えたら赤くする（保存の前に `validatePushFields` が断る）。
	-->
	<label class="field">
		受信のタイムアウト（ms）
		<input
			type="text"
			inputmode="numeric"
			bind:value={form.staleTimeoutMs}
			placeholder={String(DEFAULT_STALE_TIMEOUT_MS)}
			disabled={readOnly}
		/>
		<span class="hint"
			>送り手からの最後の通信からこの時間が過ぎると、この接続のタグは Bad になります（空欄は既定の
			10,000 ms、1,000〜3,600,000）。</span
		>
		{#if errors.staleTimeoutMs}<span class="err">{errors.staleTimeoutMs}</span>{/if}
	</label>
	<label class="field checkbox wide">
		<input type="checkbox" bind:checked={form.bridgeEnabled} disabled={readOnly} />
		DB ブリッジ（PostgreSQL を読んで送る）
	</label>
	{#if form.bridgeEnabled}
		<div class="bridge wide">
			<label class="field">
				ブリッジが読む PostgreSQL 接続
				<select bind:value={form.bridgeDbConnectionId} disabled={readOnly}>
					<option value="">（選択してください）</option>
					{#each postgresConnections as pg (pg.id)}
						<option value={String(pg.id)}>{pg.name}</option>
					{/each}
				</select>
				{#if postgresConnections.length === 0}
					<span class="hint">PostgreSQL（DB Source）接続を先に作ってください。</span>
				{/if}
				{#if errors.bridgeDbConnectionId}<span class="err">{errors.bridgeDbConnectionId}</span>{/if}
			</label>
			{#each form.bridgeQueries as query, index (index)}
				{@const queryLoad = bridgeLoadEstimate?.queries[index]}
				<fieldset class="bridge-query">
					<legend>クエリ {index + 1}</legend>
					<label class="field">
						クエリの名前
						<input type="text" bind:value={query.name} disabled={readOnly} />
						{#if errors[bridgeQueryErrorKey(index, 'name')]}<span class="err"
								>{errors[bridgeQueryErrorKey(index, 'name')]}</span
							>{/if}
					</label>
					<label class="field">
						周期
						<select bind:value={query.periodMs} disabled={readOnly}>
							{#each BRIDGE_PERIOD_OPTIONS as ms (ms)}
								<option value={ms}>{ms} ms</option>
							{/each}
						</select>
					</label>
					<label class="field">
						SQL
						<textarea
							rows="3"
							bind:value={query.sql}
							placeholder="SELECT temp, run FROM line1_now"
							disabled={readOnly}></textarea>
						{#if errors[bridgeQueryErrorKey(index, 'sql')]}<span class="err"
								>{errors[bridgeQueryErrorKey(index, 'sql')]}</span
							>{/if}
					</label>
					<label class="field">
						列とキーの対応（1 行に 1 つ「列名=キー」）
						<textarea
							rows="3"
							bind:value={query.columnsText}
							placeholder="temp=line1.temp&#10;run=line1.run"
							disabled={readOnly}></textarea>
						{#if errors[bridgeQueryErrorKey(index, 'columns')]}<span class="err"
								>{errors[bridgeQueryErrorKey(index, 'columns')]}</span
							>{/if}
					</label>
					{#if queryLoad}
						<span class="hint"
							>このクエリ: {queryLoad.items} 件・1 周期 {queryLoad.messagesPerCycle} メッセージ</span
						>
					{/if}
					{#if !readOnly}
						<button type="button" class="secondary" onclick={() => removeBridgeQuery(index)}
							>このクエリを外す</button
						>
					{/if}
				</fieldset>
			{/each}
			{#if !readOnly}
				<button type="button" class="secondary" onclick={addBridgeQuery}>クエリを追加</button>
			{/if}
			{#if bridgeLoadEstimate}
				<p
					class="bridge-load"
					class:over={!bridgeLoadEstimate.withinLimit}
					data-testid="bridge-load-estimate"
				>
					送る量の見積もり: 約 {bridgeLoadEstimate.messagesPerSecText} メッセージ/秒・{bridgeLoadEstimate.itemsPerSecText}
					件/秒（上限の 8 割: {REGISTRATION_MESSAGES_PER_SEC} メッセージ・{REGISTRATION_ITEMS_PER_SEC.toLocaleString(
						'ja-JP'
					)} 件）
				</p>
				{#if bridgeLoadEstimate.message}
					<span class="err" role="alert">{bridgeLoadEstimate.message}</span>
				{/if}
			{/if}
			{#if errors.bridge && errors.bridge !== bridgeLoadEstimate?.message}
				<span class="err">{errors.bridge}</span>
			{/if}
		</div>
	{/if}
{/snippet}

{#snippet ingestInfoBlock()}
	<!--
		R2a-2e（§5・§6）: 送り手向けの URL とスコープ。接続は id で指す（Q9 = (b)）ので、
		改名しても変わらない。保存前（作成ウィザード）は id が無いので案内だけ。
	-->
	<section class="ingest-info" aria-label="送り手向けの URL とスコープ">
		<h3>送り手向けの URL とスコープ</h3>
		{#if endpoints}
			<dl class="summary">
				<dt>REST（一括送信）</dt>
				<dd><code data-testid="ingest-rest-url">POST {endpoints.restUrl}</code></dd>
				<dt>WebSocket</dt>
				<dd><code data-testid="ingest-ws-url">{endpoints.wsUrl}</code></dd>
				<dt>API キーのスコープ</dt>
				<dd><code data-testid="ingest-scope">{endpoints.scope}</code></dd>
			</dl>
			<p class="hint">
				送り手の API キーは「API キー」の画面で、取り込み（この接続）を選んで発行します（スコープ
				{endpoints.scope}）。URL とスコープは接続の id で決まるので、名前を変えても変わりません。
				構成パッケージで別の Hub に移すと id が変わり、キーも移りません。
			</p>
		{:else}
			<p class="hint">
				作成すると、ここに送り手向けの URL（/api/v1/ingest/接続 id/…）と API キーのスコープ
				ingest:#接続 id が出ます。
			</p>
		{/if}
	</section>
{/snippet}

{#snippet ingestStatusBlock()}
	<!--
		R2a-2e（§9）: 受信の状態。`GET /api/status` の `ingest` 節（資格情報と値は
		含まない）。送り手の `status` の文は画面でも 1 KiB に切り詰める。
	-->
	<section class="ingest-status" aria-label="受信の状態" data-testid="ingest-status">
		<h3>受信の状態</h3>
		{#if !ingestStatusLoaded}
			<p class="hint">読み込み中…</p>
		{:else if ingestStatus === null}
			{#if ingestStatusError}
				<p class="err">状態を読めませんでした: {ingestStatusError}</p>
			{:else}
				<p class="hint">まだ収集の構成に入っていません（保存の直後は反映を待ってください）。</p>
			{/if}
		{:else}
			<p>
				<span class="state-chip {ingestLinkClass(ingestStatus)}" data-testid="ingest-link"
					>{ingestLinkLabel(ingestStatus)}</span
				>
				{#if ingestStatus.link !== 'live' && ingestStatus.link_detail}
					<span class="hint">{ingestStatus.link_detail}</span>
				{/if}
			</p>
			<dl class="summary">
				<dt>最後の通信</dt>
				<dd>{formatIngestTime(ingestStatus.last_seen_at)}</dd>
				<dt>収集</dt>
				<dd>{ingestStatus.collecting ? '収集中' : '停止中（値は受けますが記録しません）'}</dd>
				{#if ingestStatus.bridge}
					<dt>世代（epoch）</dt>
					<dd>{ingestStatus.epoch}</dd>
				{/if}
				<dt>受信のタイムアウト</dt>
				<dd>{ingestStatus.stale_timeout_ms.toLocaleString('ja-JP')} ms</dd>
				<dt>保持している値</dt>
				<dd>{ingestStatus.held_values} 件</dd>
				<dt>受け付けた / 断った</dt>
				<dd>
					{ingestStatus.accepted.toLocaleString('ja-JP')} 件 / {ingestStatus.rejected.toLocaleString(
						'ja-JP'
					)} 件
				</dd>
				<dt>上限で断ったメッセージ</dt>
				<dd>
					頻度 {ingestStatus.rate_limited.toLocaleString('ja-JP')} / 件数 {ingestStatus.too_many_items.toLocaleString(
						'ja-JP'
					)}
				</dd>
				<dt>送り手</dt>
				<dd>
					{#if ingestStatus.senders.length === 0}
						（まだありません）
					{:else}
						<ul class="plain-list">
							{#each ingestStatus.senders as sender (sender.transport)}
								<li>{describeIngestSender(sender)}</li>
							{/each}
						</ul>
					{/if}
				</dd>
				{#if ingestStatus.last_source_time !== null}
					<dt>送り手の時刻（最大）</dt>
					<dd>{formatIngestTime(ingestStatus.last_source_time)}</dd>
				{/if}
				{#if ingestStatus.unknown_keys.length > 0}
					<dt>未知のキー（直近）</dt>
					<dd class="unknown-keys">{ingestStatus.unknown_keys.join('、')}</dd>
				{/if}
				{#if ingestStatus.sender_status}
					<dt>送り手の状態</dt>
					<dd class="sender-status">{truncateSenderStatus(ingestStatus.sender_status)}</dd>
				{/if}
			</dl>
			{#if ingestStatusError}
				<p class="hint">最新の状態を読めませんでした（前回の内容を表示しています）。</p>
			{/if}
		{/if}
	</section>
{/snippet}

{#snippet testConnectionBlock()}
	<div class="test-connection">
		<button
			type="button"
			class="test-btn"
			onclick={runConnectionTest}
			disabled={testState.testing || driversBlocking}
		>
			{#if testState.testing}<span class="spinner" aria-hidden="true"></span>{/if}
			接続テスト
		</button>
		{#if testState.testing}
			<span class="test-result testing">テスト中…</span>
		{:else if testState.result}
			{#if testState.result.ok}
				<span class="test-result ok">接続成功（応答 {testState.result.elapsedMs}ms）</span>
			{:else}
				<span class="test-result error"
					>{testState.result.error?.message ?? '接続に失敗しました'}</span
				>
			{/if}
		{/if}
	</div>
{/snippet}

{#snippet dbTestConnectionBlock()}
	<!--
		S1（docs/banto-hub-external-db-design.md §4.6、実装指示3）: 保存済み
		postgres 接続専用の接続テスト。T12 の`testConnectionBlock`（保存前の
		フォーム値をそのまま送る）とは違い、`POST /api/plc-connections/{id}
		/test`は保存済みの行しか対象にできない（サーバー側がパスワードを
		DBから読み直す設計 - パスワードをフォームへ再表示しないため）ので、
		未保存（新規作成ウィザード中）はボタンを disabled にし「保存後に
		テストできます」のヒントを出す。
	-->
	<div class="test-connection">
		<button
			type="button"
			class="test-btn"
			onclick={runDbConnectionTest}
			disabled={!connection || dbTestState.testing}
		>
			{#if dbTestState.testing}<span class="spinner" aria-hidden="true"></span>{/if}
			接続テスト
		</button>
		{#if !connection}
			<span class="hint">保存後にテストできます</span>
		{:else if dbTestState.testing}
			<span class="test-result testing">テスト中…</span>
		{:else if dbTestState.result}
			{#if dbTestState.result.ok}
				<span class="test-result ok"
					>接続成功{dbTestState.result.serverVersion
						? `（${dbTestState.result.serverVersion}）`
						: ''}</span
				>
			{:else}
				<span class="test-result error">{dbTestState.result.error ?? '接続に失敗しました'}</span>
			{/if}
		{/if}
	</div>
{/snippet}

{#snippet confirmSummary()}
	<dl class="summary">
		<dt>名前</dt>
		<dd>{form.name || '（未入力）'}</dd>
		<dt>プロトコル</dt>
		<dd>{protocolChoices.find((o) => o.value === form.protocol)?.label ?? form.protocol}</dd>
		{#if form.protocol === PUSH_PROTOCOL}
			<dt>受信のタイムアウト</dt>
			<dd>
				{String(form.staleTimeoutMs).trim() === ''
					? `既定（${DEFAULT_STALE_TIMEOUT_MS.toLocaleString('ja-JP')} ms）`
					: `${form.staleTimeoutMs} ms`}
			</dd>
			<dt>DB ブリッジ</dt>
			<dd>
				{#if form.bridgeEnabled}
					{postgresConnections.find((c) => String(c.id) === form.bridgeDbConnectionId)?.name ??
						'（接続未選択）'} を読む・クエリ {form.bridgeQueries.length} 本{#if bridgeLoadEstimate}（約
						{bridgeLoadEstimate.messagesPerSecText} メッセージ/秒・{bridgeLoadEstimate.itemsPerSecText}
						件/秒）{/if}
				{:else}
					なし（スクリプトなどの送り手が送る）
				{/if}
			</dd>
		{:else if form.protocol === POSTGRES_PROTOCOL}
			<dt>ホスト</dt>
			<dd>{form.host || '（未入力）'}:{form.port}</dd>
			<dt>データベース</dt>
			<dd>{form.database || '（未入力）'}</dd>
			<dt>ユーザー名</dt>
			<dd>{form.username || '（未入力）'}</dd>
			<dt>パスワード</dt>
			<dd>{form.password ? '入力あり' : '（未入力・後で設定できます）'}</dd>
		{:else if driver}
			{#each summarizeDriverValues(driver.settingsSchema.fields, form.driver.values) as row (row.label)}
				<dt>{row.label}</dt>
				<dd>{row.text}</dd>
			{/each}
		{/if}
		<dt>有効</dt>
		<dd>{form.enabled ? 'はい' : 'いいえ'}</dd>
		{#if driver}
			<dt>シミュレーション</dt>
			<dd>{form.simulation ? '⚠ シミュレーション中' : 'いいえ'}</dd>
		{/if}
	</dl>
{/snippet}

{#if isCreate}
	<Modal
		open={open && isCreate}
		title={drawerTitle}
		{onRequestClose}
		onclose={onClose}
		width="560px"
		{dirty}
		onBlockedClose={notifyBlockedClose}
		{focusFallback}
	>
		<ol class="wizard-steps" aria-label="作成手順">
			<li class:active={step === 1} class:done={step > 1}>1. 識別</li>
			<li class:active={step === 2} class:done={step > 2}>2. プロトコルと接続先</li>
			<li class:active={step === 3}>3. 確認</li>
		</ol>

		{#if step === 1}
			{@render nameField()}
		{:else if step === 2}
			{@render destinationFields()}
		{:else}
			{@render confirmSummary()}
			{#if form.protocol === PUSH_PROTOCOL}
				{@render ingestInfoBlock()}
			{:else if form.protocol === POSTGRES_PROTOCOL}
				{@render dbTestConnectionBlock()}
			{:else if driver}
				{@render testConnectionBlock()}
			{/if}
		{/if}

		<div class="wizard-actions">
			{#if step > 1}
				<button type="button" class="secondary" onclick={goBack} disabled={saving}>戻る</button>
			{/if}
			{#if step === 1}
				<button type="button" onclick={goNext} disabled={!canAdvanceFromStep1}>次へ</button>
			{:else if step === 2}
				<button type="button" onclick={goNext} disabled={!canAdvanceFromStep2 || driversBlocking}>
					次へ
				</button>
			{:else}
				<button type="button" onclick={handleCreate} disabled={saving || driversBlocking}>
					作成
				</button>
			{/if}
		</div>
	</Modal>
{:else}
	<Drawer
		open={open && !isCreate}
		title={drawerTitle}
		{onRequestClose}
		onclose={onClose}
		width="480px"
		{dirty}
		onBlockedClose={notifyBlockedClose}
		{focusFallback}
	>
		{@render nameField()}
		{@render destinationFields()}
		{#if savedIsPush}
			{@render ingestInfoBlock()}
			{@render ingestStatusBlock()}
		{/if}
		{#if !readOnly}
			{#if form.protocol === PUSH_PROTOCOL}
				<!-- R2a-2e: push は接続テストを持たない（受信の状態を上に出す）。 -->
			{:else if form.protocol === POSTGRES_PROTOCOL}
				{@render dbTestConnectionBlock()}
			{:else if driver}
				{@render testConnectionBlock()}
			{/if}
			<div class="actions">
				<button type="button" onclick={handleSave} disabled={saving || deleting || driversBlocking}>
					保存
				</button>
				<button type="button" class="danger" onclick={handleDelete} disabled={saving || deleting}>
					削除
				</button>
			</div>
		{/if}
	</Drawer>
{/if}

<style>
	.form-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
		gap: 0.75rem;
		margin-bottom: 0.75rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		margin-bottom: 0.75rem;
	}

	.field.checkbox {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
	}

	.field input,
	.field select {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
	}

	.field.checkbox input {
		width: auto;
	}

	.hint {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	.sim-hint {
		grid-column: 1 / -1;
		margin-top: -0.4rem;
		color: var(--banto-warning);
	}

	.drivers-error {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.password-status {
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		align-self: flex-end;
	}

	.password-status.unreadable {
		color: var(--banto-warning);
		font-weight: 600;
	}

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
	}

	/* R2a-2e: push の欄・送り手向けの情報・受信の状態。 */
	.wide {
		grid-column: 1 / -1;
	}

	.bridge {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.bridge-query {
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		padding: 0.5rem 0.75rem;
		margin: 0;
	}

	.bridge-query legend {
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.field textarea {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-family: var(--banto-font-mono, monospace);
	}

	.bridge-load {
		margin: 0;
		font-size: 0.8rem;
		color: var(--banto-text);
	}

	.bridge-load.over {
		color: var(--banto-danger);
		font-weight: 600;
	}

	.ingest-info,
	.ingest-status {
		margin-bottom: 0.75rem;
	}

	.ingest-info h3,
	.ingest-status h3 {
		font-size: 0.85rem;
		margin: 0 0 0.5rem;
	}

	.ingest-info code {
		word-break: break-all;
	}

	.state-chip {
		display: inline-block;
		padding: 0.1rem 0.5rem;
		border-radius: var(--banto-radius);
		font-size: 0.8rem;
		font-weight: 600;
	}

	.state-chip.ok {
		color: var(--banto-success, #1a7f37);
		border: 1px solid var(--banto-success, #1a7f37);
	}

	.state-chip.warn {
		color: var(--banto-warning, #8a5a00);
		border: 1px solid var(--banto-warning, #8a5a00);
	}

	.state-chip.muted {
		color: var(--banto-text-muted);
		border: 1px solid var(--banto-border);
	}

	.plain-list {
		margin: 0;
		padding-left: 1rem;
	}

	.unknown-keys,
	.sender-status {
		word-break: break-all;
	}

	.wizard-steps {
		display: flex;
		gap: 0.5rem;
		list-style: none;
		margin: 0 0 1rem;
		padding: 0;
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.wizard-steps li {
		padding: 0.3rem 0.6rem;
		border-radius: var(--banto-radius);
		border: 1px solid var(--banto-border);
	}

	.wizard-steps li.active {
		border-color: var(--banto-primary);
		color: var(--banto-primary);
		font-weight: 600;
	}

	.wizard-steps li.done {
		color: var(--banto-text);
	}

	.wizard-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.75rem;
		margin-top: 1rem;
	}

	.summary {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 0.35rem 0.75rem;
		margin: 0 0 1rem;
		font-size: 0.85rem;
	}

	.summary dt {
		color: var(--banto-text-muted);
	}

	.summary dd {
		margin: 0;
		color: var(--banto-text);
	}

	.actions {
		display: flex;
		gap: 0.75rem;
		margin-top: 1rem;
	}

	.test-connection {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin-bottom: 0.75rem;
	}

	.test-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		background: var(--banto-bg);
		color: var(--banto-text);
		border: 1px solid var(--banto-border);
		font-weight: 500;
	}

	.test-btn:hover:not(:disabled) {
		background: var(--banto-surface);
	}

	.spinner {
		width: 0.85rem;
		height: 0.85rem;
		border: 2px solid color-mix(in srgb, var(--banto-text) 25%, transparent);
		border-top-color: var(--banto-text);
		border-radius: 50%;
		animation: spin 0.7s linear infinite;
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

	.test-result {
		font-size: 0.8rem;
	}

	.test-result.testing {
		color: var(--banto-text-muted);
	}

	.test-result.ok {
		color: var(--banto-success, #1a7f37);
		font-weight: 600;
	}

	.test-result.error {
		color: var(--banto-danger);
		font-weight: 600;
	}

	button {
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	button:hover:not(:disabled) {
		background: var(--banto-primary-hover);
	}

	button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text-muted);
	}

	button.secondary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}

	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}

	button.danger:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}
</style>
