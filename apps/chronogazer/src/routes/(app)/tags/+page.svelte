<script lang="ts">
	/**
	 * 「記録」ページ（#383 段階2a / R1-B、recorder-requirements.md §6）。
	 *
	 * **3 ペイン（#1179 PR4b、2026-11-18、docs/chronogazer/chronogazer-ui-improvement-proposal.md
	 * §2.1）**: HUB のタグページ（#1179 PR2a〜PR3c）と同じ banto 系の型にした。
	 * - 左 = `RecordTree`（`@banto/tree-svelte` の `BantoTree`。すべて / 接続先 /
	 *   収集グループ。記録項目はツリーに出さない）。上のツールバーに「接続先を追加」
	 *   「収集グループを追加」。枠は `@banto/ui` の `ThreePaneLayout`（狭幅では左右が
	 *   ドロワー。狭幅の判定はビューポート = `mobileNavStore.isNarrow`、HUB と同じ）。
	 * - 中央 = `@banto/ui` の `Tabs`。「記録項目」（ツリーの選択で絞った一覧と
	 *   「記録項目を追加」「Hub から追加」）と「設定」（接続先・収集グループの
	 *   編集と削除。接続先かグループを選んでいて、かつ編集者以上のときだけ）。
	 * - 右 = 記録項目の詳細（編集・しきい値・削除）か、作成フォーム（記録項目・
	 *   接続先・収集グループ）のどれか 1 つ。作成フォームを右に出すのは、中央の
	 *   一覧を見ながら作れるようにするため（HUB の右ペインの新規作成と同じ）。
	 * - 選んだ対象とタブは URL（`?target=…&tab=settings`、`#lib/banto/recordsView.ts`）
	 *   に持ち、再読み込み・戻る操作で同じ画面に戻る。
	 * - 記録項目の追加は収集グループを選んでいるときだけ（HUB の 2026-11-18
	 *   オーナー決定と同じ）。
	 * - 未保存の確認: 右ペインか「設定」タブのフォームが未保存のまま、対象・タブ・
	 *   行を変える、右ペインを閉じる、戻る・別ページへ移るときに確かめる。
	 * 以前は接続 / 収集グループ / タグの 3 セクションを縦に並べていた（タブ部品が
	 * 無かったため）。`@banto/ui` に `ThreePaneLayout`・`Tabs` が入ったので置き換えた。
	 * 削除は `window.confirm`（users 画面と同じ出し方）。中央モーダルは「Hub から
	 * 追加」（`HubImportDialog.svelte`）だけ。
	 *
	 * **Hub 経由の接続（#383 段階3 P4）**: Hub 接続（`protocol: 'hub'`）は
	 * 作成フォームでは作らず、「Hub から追加」がサーバー側で暗黙に作る。一覧では
	 * 読み取り専用（ホスト・ポートを持たない。接続先と API キーは「設定 > Hub接続」）
	 * で、「設定」タブには「有効」の切り替えと削除だけを出す（配下にグループがあれば削除は
	 * サーバーが断る）。「有効」は PLC 接続と同じ更新・同じ未保存の確認で、無効にすると
	 * その接続の Hub のタグは購読も収集もしない（2026-10-11 オーナー決定）。接続の行に
	 * 版（楽観ロック）は無い - PLC 接続の編集と同じ。Hub 経由のタグの編集フォームはスケーリングの欄を出さない（Hub は
	 * 工学値を送る。サーバーも断る）。
	 *
	 * viewer は閲覧のみ（`canWriteResources` = editor 以上の判定、
	 * `#lib/permissions.ts` 既存）: 追加のボタン・「設定」タブ・右ペイン（行を
	 * 選んでも開かない）を出さない。
	 *
	 * 収集グループの周期は `banto_tags::ALLOWED_PERIOD_MS` と一致する固定選択
	 * 肢（`tagRegistryAdmin.ts`の`ALLOWED_PERIOD_MS`）。
	 *
	 * 一覧は全件取得のクライアントモード（`BantoGrid`のページングは使わない）
	 * - 256タグ程度なら現実的（R0のv1目標、指示書どおりこのPRではこれで良い。
	 * サーバーモードのページングはR1-C以降の実データ時に検討）。
	 *
	 * **フィールド名の接頭辞について（E2Eで実際に踏んだ事故）**: 縦並びの
	 * 3 セクションだった頃は、接続・グループ・タグの「新規作成」と「編集」の
	 * 最大 6 個の `BantoForm` が同じページに同時に描かれていた。`@banto/forms`の
	 * 各フィールドは `id={def.name}`（例: `"name"`）を持つため、スキーマの
	 * フィールド名をそのまま使うと `id="name"` が重複し、`<label for="name">` が
	 * 最初の1個にしか紐付かず、残りはラベル無しになる（E2E で「収集グループの
	 * 名前欄が見つからない」という形で踏んだ）。3 ペインにしてからも、「設定」
	 * タブ（接続先・グループの編集）と右ペイン（記録項目の編集・作成フォーム）は
	 * 同時に描かれるので、各エンティティ×モードごとに一意な接頭辞（`plcCreate`/
	 * `plcEdit`/`groupCreate`/`groupEdit`/`tagCreate`/`tagEdit`）をスキーマの
	 * フィールド名自体に付け、`to*Input`/`*FormValues` 側で wire のフィールド名と
	 * 相互変換する仕組みは残す。
	 *
	 * **接続単位シミュレーション（#413、2026-09-23 オーナー決定）**: 接続の
	 * フォームに切替、ツリーに「シミュレーション」の印、「設定」タブに「動作」
	 * （シミュレーション接続は「値は記録されません」）を出す（R2b-5c: 「値が動かないタグ」の一覧は、シミュレーション
	 * が値の層に移って全タグが動くので無くなった）。一覧・フォームはレジストリの
	 * 値で、**走っている収集への反映は「収集を再起動」**（保存時のトーストで
	 * 案内する）。走っている収集が実際にシミュレータ相手かは
	 * `/settings/collect` の接続ごとの状態に出る。
	 *
	 * **収集の開始時に外される設定（#414 段階2、2026-09-23 オーナー決定）**:
	 * 保存時の検証（#418）より前に入った不正なタグなどは、収集の開始時に
	 * 外されて残りだけが動く。それを編集中に分かるよう、ページの先頭に一覧を
	 * 出す（収集が走っていなくても出す）。判定は画面では行わない
	 * （`listConfigExclusions` = 収集の開始と同じ Rust の組み立ての結果）。
	 * グリッドの列ではなく一覧にしたのは、接続・グループ・タグの 3 つの単位を
	 * 1 か所で親から辿れるようにするため（接続が外れると配下のグループ・タグも
	 * 外れ、その理由は親の名前を指す）。
	 */
	import { untrack } from 'svelte';
	import { page } from '$app/state';
	import { afterNavigate, beforeNavigate, goto, type BeforeNavigate } from '$app/navigation';
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import {
		BantoForm,
		createFormStore,
		isSamePath,
		UnsavedChangesNotice,
		type FormSchema
	} from '@banto/forms';
	import { Tabs, ThreePaneLayout } from '@banto/ui';
	import { guardUnsavedChanges, UNSAVED_NOTICE } from '#lib/unsavedChanges.js';
	import { mobileNavStore } from '#lib/mobileNav.svelte.js';
	import RecordTree from '#lib/components/RecordTree.svelte';
	import {
		formatPeriodMs,
		recordTargetNodeId,
		recordTargetOf,
		type RecordTreeNodeData
	} from '#lib/components/recordTree.js';
	import {
		filterRecordsByTarget,
		MISSING_RECORD_TARGET_MESSAGE,
		parseRecordsView,
		recordsViewSearch,
		resolveRecordsView,
		sameRecordTarget,
		sameRecordsView,
		type RecordTarget,
		type RecordsTab,
		type RecordsView
	} from '#lib/banto/recordsView.js';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '#lib/toast.svelte.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { canWriteResources } from '#lib/permissions.js';
	import {
		listPlcConnections,
		listDrivers,
		createPlcConnection,
		updatePlcConnection,
		deletePlcConnection,
		listCollectionGroups,
		createCollectionGroup,
		updateCollectionGroup,
		deleteCollectionGroup,
		listTags,
		createTag,
		updateTag,
		deleteTag,
		listConfigExclusions,
		ALLOWED_PERIOD_MS,
		type PlcConnection,
		type PlcConnectionInput,
		type DriverInfo,
		type CollectionGroup,
		type CollectionGroupInput,
		type Tag,
		type ExclusionView
	} from '#lib/banto/tagRegistryAdmin.js';
	import { exclusionUnitLabel } from '#lib/banto/collectAdmin.js';
	import DriverSettingsForm from '#lib/components/DriverSettingsForm.svelte';
	import {
		buildDriverSettings,
		defaultDriverState,
		driverStateFromSettings,
		mapSettingsFieldErrors,
		switchDriverState,
		validateDriverValues
	} from '#lib/banto/driverSettingsForm.js';
	import HubImportDialog from './HubImportDialog.svelte';
	import {
		getTagThresholds,
		updateTagThresholds,
		type TagThresholds
	} from '#lib/banto/tagThresholdsAdmin.js';
	import {
		runGuardedSave,
		runGuardedDelete,
		schemaWireFields,
		splitServerFieldErrors,
		joinFieldErrorMessages,
		listSectionView,
		showsRetry,
		listRows,
		createFormGate,
		runGuardedListLoad,
		buildTagInput,
		tagFormValues,
		buildThresholdsInput,
		thresholdFormValues,
		THRESHOLD_WIRE_FIELDS,
		THRESHOLDS_SAVED_MESSAGE,
		isRevisionConflict,
		connectionModeLabel,
		connectionOptionLabel,
		tagDataTypeOptions,
		connectionSavedMessage,
		hubConnectionInput,
		hubConnectionSavedMessage,
		configExclusionsView,
		coverageReloadStarted,
		coverageReloadSettled,
		type SaveGuardToken,
		type DeleteGuardToken,
		type ListLoadState
	} from './tagsPageLogic';

	const canWrite = $derived(canWriteResources(sessionStore.role));

	/**
	 * #391 レビュー C: `ProviderError.message` は検証エラーでは一律
	 * `"validation failed"`（`@banto/admin-core`の`errors.ts`の
	 * `describe()`）になり、削除拒否理由のような具体的な理由が消える
	 * （例: 「この接続を使用している収集グループがN件あるため削除
	 * できません」が `field_errors` にしか無い）。`joinFieldErrorMessages`
	 * （`tagsPageLogic.ts`）で連結して返す。
	 */
	function errorMessage(err: unknown): string {
		if (isProviderError(err)) {
			return err.body.kind === 'validation'
				? joinFieldErrorMessages(err.body.field_errors)
				: err.message;
		}
		return String(err);
	}

	/**
	 * サーバーの検証エラー（`banto_tags`/`chronogazer_core::rest`の
	 * `FieldError.field` は wire のフィールド名そのまま、例: `"name"`/
	 * `"periodMs"`）をフォームのフィールドへマッピングする。このモジュール
	 * doc comment の「フィールド名の接頭辞について」のとおり、実際の
	 * フォームフィールド名は `${prefix}${capitalize(wireField)}`
	 * （例: `"plcCreateName"`）。
	 *
	 * #391 レビュー B: 以前はスキーマに無いフィールド（バックエンドの
	 * `Scaling::from_parts()` が返す `field: "scaling"` など）でも
	 * 確認せずそのまま `setServerErrors` に渡していたため、画面にもトーストにも
	 * 何も出ずに保存できない理由が分からなくなっていた。`knownWireFields`
	 * （呼び出し側がスキーマから渡す）と突き合わせる `splitServerFieldErrors`
	 * （`tagsPageLogic.ts`）を通し、フォームに出せない分は必ずトーストへ
	 * フォールバックさせる - 「エラーが無表示」を構造的に起こさない。
	 */
	function applyServerErrors(
		prefix: string,
		knownWireFields: readonly string[],
		err: unknown,
		store: ReturnType<typeof createFormStore>
	): void {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const { formErrors, toastMessages } = splitServerFieldErrors(
				prefix,
				knownWireFields,
				err.body.field_errors
			);
			if (formErrors.length > 0) store.setServerErrors(formErrors);
			for (const message of toastMessages) toastStore.push('error', message);
			return;
		}
		toastStore.push('error', errorMessage(err));
	}

	// --- PLC接続 -----------------------------------------------------------

	const PLC_CREATE = 'plcCreate';
	const PLC_EDIT = 'plcEdit';

	let drivers = $state<DriverInfo[] | null>(null);
	let driversError: string | null = $state(null);
	const protocolOptions = $derived(
		(drivers ?? [])
			.filter((driver) => driver.id !== 'hub')
			.map((driver) => ({ value: driver.id, label: driver.displayName }))
	);
	const driverFor = (protocol: string): DriverInfo | undefined =>
		(drivers ?? []).find((driver) => driver.id === protocol && driver.id !== 'hub');

	async function reloadDrivers(): Promise<void> {
		try {
			const catalog = await listDrivers();
			drivers = catalog.filter((driver) => driver.id !== 'hub');
			driversError = null;
			if (!createConnectionStore.isDirty) {
				createConnectionStore = createFormStore(connectionSchema(PLC_CREATE));
			}
		} catch (err) {
			driversError = errorMessage(err);
		}
	}

	/** `prefix` はこのモジュール doc comment の「フィールド名の接頭辞について」参照（例: `plcCreate`/`plcEdit`）。 */
	function connectionSchema(prefix: string): FormSchema {
		return {
			fields: [
				{ name: `${prefix}Name`, label: '名前', type: 'text', required: true, min: 1, max: 100 },
				{
					name: `${prefix}Protocol`,
					label: 'プロトコル',
					type: 'select',
					required: true,
					default: protocolOptions[0]?.value ?? '',
					options: protocolOptions
				},
				{ name: `${prefix}Enabled`, label: '有効', type: 'checkbox', default: true },
				// #413: 接続単位シミュレーション。ラベルに「記録されない」ことを
				// 書くのは、チェックを入れる時点で分かるようにするため。
				{
					name: `${prefix}Simulation`,
					label:
						'シミュレーション（実機の代わりに内蔵シミュレータへ接続。値はデータファイルに記録されません）',
					type: 'checkbox',
					default: false
				}
			]
		};
	}

	function toPlcConnectionInput(
		prefix: string,
		values: Record<string, unknown>,
		settings: Record<string, unknown>
	): PlcConnectionInput {
		return {
			name: String(values[`${prefix}Name`] ?? ''),
			protocol: String(values[`${prefix}Protocol`] ?? ''),
			enabled: Boolean(values[`${prefix}Enabled`]),
			settings,
			simulation: Boolean(values[`${prefix}Simulation`])
		};
	}

	/** 編集フォームの共通項目の初期値。ドライバの値はスキーマから復元する。 */
	function connectionFormValues(prefix: string, conn: PlcConnection): Record<string, unknown> {
		return {
			[`${prefix}Name`]: conn.name,
			[`${prefix}Protocol`]: conn.protocol,
			[`${prefix}Enabled`]: conn.enabled,
			[`${prefix}Simulation`]: conn.simulation
		};
	}

	// スキーマに実在する wire フィールド名一覧（`applyServerErrors`が
	// `scaling`のようなスキーマ外のフィールドをトーストへ振り分けられる
	// ようにする）。CREATE/EDIT どちらのプレフィックスで計算しても剥がした
	// 後の wire 名は同じなので、ここでは PLC_CREATE 側で一度だけ計算する。
	const CONNECTION_WIRE_FIELDS = schemaWireFields(PLC_CREATE, connectionSchema(PLC_CREATE).fields);

	function applyConnectionErrors(
		prefix: string,
		err: unknown,
		store: ReturnType<typeof createFormStore>,
		driver: DriverInfo,
		creating: boolean
	): void {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const fieldErrors = Object.fromEntries(
				(err.body.field_errors ?? []).map(({ field, message }) => [field, message])
			);
			const mapped = mapSettingsFieldErrors(
				driver.settingsSchema.fields.map((field) => field.key),
				fieldErrors
			);
			if (creating) createDriverErrors = mapped.fields;
			else editDriverErrors = mapped.fields;
			const { formErrors, toastMessages } = splitServerFieldErrors(
				prefix,
				CONNECTION_WIRE_FIELDS,
				Object.entries(mapped.other).map(([field, message]) => ({ field, message }))
			);
			if (formErrors.length > 0) store.setServerErrors(formErrors);
			for (const message of toastMessages) toastStore.push('error', message);
			return;
		}
		applyServerErrors(prefix, CONNECTION_WIRE_FIELDS, err, store);
	}

	// `null` は「まだ読めていない」。`[]`（読めた結果0件）とは**別物**として
	// 扱う（#394レビュー P1-3。同じアプリのHub側 `HubView.tags` と同じ規律で、
	// 判断は `tagsPageLogic.ts` の純関数に出してある）。
	let connections: PlcConnection[] | null = $state(null);
	let connectionsError: string | null = $state(null);
	let connectionsLoading = $state(false);
	const connectionsState: ListLoadState<PlcConnection> = $derived({
		items: connections,
		error: connectionsError
	});
	const connectionRows = $derived(listRows(connectionsState));
	const connectionsView = $derived(listSectionView(connectionsState));
	const connectionsRetry = $derived(showsRetry(connectionsState));
	/** #413: レジストリ上にシミュレーション接続が 1 本でもあるか（注記の出し分け）。 */
	const hasSimulationConnection = $derived(connectionRows.some((c) => c.simulation));

	/**
	 * 一覧ごとの再取得の世代（レビュー P2-C）。`$state` にしないのは描画に
	 * 使わないため（照合だけに使うカウンタ）。再取得のたびに進め、応答の
	 * 適用時に `runGuardedListLoad` が照合する。
	 */
	let connectionsGeneration = 0;

	async function reloadConnections(): Promise<void> {
		// レビュー P2-C: 作成・保存の直後に `GET` が 2 本飛ぶと着順は発行順と
		// 一致しない。この再取得の世代を控え、応答の適用時に照合する。
		connectionsGeneration += 1;
		const generation = connectionsGeneration;
		connectionsLoading = true;
		connectionsError = null;
		const outcome = await runGuardedListLoad(
			generation,
			listPlcConnections(),
			() => connectionsGeneration
		);
		switch (outcome.kind) {
			case 'applied':
				connections = outcome.items;
				// #414 段階2: どの一覧が変わっても「開始時に外される設定」の判定は変わる。
				void reloadConfigExclusions();
				break;
			case 'error':
				// 失敗を「0件」に潰さない: `connections` には触らない（未読込なら
				// `null` のまま、既に読めていたなら前回の行を残す）。トーストは
				// 消えてしまうので、画面にも失敗と再試行の導線を残す。
				connectionsError = errorMessage(outcome.err);
				toastStore.push('error', connectionsError);
				break;
			case 'stale':
				// より新しい再取得が走っている。古い一覧でもエラーでも上書き
				// しない（`connectionsLoading` も落とさない - 新しい方が落とす）。
				return;
		}
		connectionsLoading = false;
	}

	// `untrack`: 初期スナップショットで十分（テンプレート側の `schema` prop は
	// 直接 `connectionSchema(...)` を呼ぶので常に最新 - Svelteの
	// `state_referenced_locally`警告を意図どおり黙らせる）。このセクションの
	// スキーマは動的な options を持たないため untrack が無くても警告は出ないが、
	// 他の2セクションと同じ書き方に揃えている。
	let createConnectionStore = $state(untrack(() => createFormStore(connectionSchema(PLC_CREATE))));
	let createDriverState = $state(defaultDriverState([]));
	let createDriverId = $state('');
	let createDriverErrors: Record<string, string> = $state({});
	let creatingConnection = $state(false);

	$effect(() => {
		const protocol = String(createConnectionStore.values[`${PLC_CREATE}Protocol`] ?? '');
		const driver = driverFor(protocol);
		if (!driver || protocol === createDriverId) return;
		const previous = driverFor(createDriverId);
		createDriverState = switchDriverState(
			previous ? { fields: previous.settingsSchema.fields, state: createDriverState } : null,
			driver.settingsSchema.fields
		);
		createDriverId = protocol;
		createDriverErrors = {};
	});

	async function handleCreateConnection(values: Record<string, unknown>): Promise<void> {
		const driver = driverFor(String(values[`${PLC_CREATE}Protocol`] ?? ''));
		if (!driver) return;
		createDriverErrors = validateDriverValues(
			driver.settingsSchema.fields,
			createDriverState.values
		);
		if (Object.keys(createDriverErrors).length > 0) return;
		creatingConnection = true;
		try {
			const created = await createPlcConnection(
				toPlcConnectionInput(
					PLC_CREATE,
					values,
					buildDriverSettings(driver.settingsSchema.fields, createDriverState.values)
				)
			);
			// #413: 応答の行（保存された値）で文言を決める - 送った値ではなく。
			toastStore.push('success', connectionSavedMessage(null, created));
			createConnectionStore = createFormStore(connectionSchema(PLC_CREATE));
			createDriverState = defaultDriverState(driver.settingsSchema.fields);
			createDriverErrors = {};
			await reloadConnections();
			// #1179 PR4b: 作った接続先をツリーで選ぶ（右の作成フォームは閉じる）。
			selectCreated('createConnection', { type: 'connection', id: created.id });
		} catch (err) {
			applyConnectionErrors(PLC_CREATE, err, createConnectionStore, driver, true);
		} finally {
			creatingConnection = false;
		}
	}

	let selectedConnection: PlcConnection | null = $state(null);
	let editConnectionStore = $state(untrack(() => createFormStore(connectionSchema(PLC_EDIT))));
	let editDriverState = $state(defaultDriverState([]));
	let editDriverId = $state('');
	let editDriverErrors: Record<string, string> = $state({});
	let savingConnection = $state(false);
	let deletingConnection = $state(false);

	/**
	 * #1179 PR4b: 「設定」タブの接続先のフォームを `conn` で作り直す（ツリーで
	 * 接続先を選んだとき、下の `$effect` から呼ぶ）。保存中・削除中に対象を
	 * 変えさせないのは `confirmViewChange` の役目（#391 レビュー A / #394 追補。
	 * 応答側の照合も不変条件として持つ理由は `runGuardedSave`/`runGuardedDelete`
	 * （`tagsPageLogic.ts`）の doc comment 参照）。
	 */
	function loadConnectionSettings(conn: PlcConnection): void {
		selectedConnection = conn;
		editConnectionStore = createFormStore(
			connectionSchema(PLC_EDIT),
			connectionFormValues(PLC_EDIT, conn)
		);
		const driver = driverFor(conn.protocol);
		editDriverState = driverStateFromSettings(driver?.settingsSchema.fields ?? [], conn.settings);
		editDriverId = driver ? conn.protocol : '';
		editDriverErrors = {};
		editHubConnectionStore = createFormStore(hubConnectionSchema(HUB_EDIT), {
			[`${HUB_EDIT}Enabled`]: conn.enabled
		});
	}

	$effect(() => {
		const protocol = String(editConnectionStore.values[`${PLC_EDIT}Protocol`] ?? '');
		const driver = driverFor(protocol);
		if (
			!selectedConnection ||
			selectedConnection.protocol === 'hub' ||
			!driver ||
			protocol === editDriverId
		)
			return;
		const previous = driverFor(editDriverId);
		editDriverState =
			!previous && protocol === selectedConnection.protocol
				? driverStateFromSettings(driver.settingsSchema.fields, selectedConnection.settings)
				: switchDriverState(
						previous ? { fields: previous.settingsSchema.fields, state: editDriverState } : null,
						driver.settingsSchema.fields
					);
		editDriverId = protocol;
		editDriverErrors = {};
	});

	/** #383 段階3 P4: 選んだ接続が Hub 接続か（編集フォームの代わりに読み取り専用の欄を出す）。 */
	const selectedConnectionIsHub = $derived.by(() => selectedConnection?.protocol === 'hub');

	// --- #383 段階3 P4: Hub 接続の「有効」（2026-10-11 オーナー決定） ---------------

	const HUB_EDIT = 'hubEdit';

	function hubConnectionSchema(prefix: string): FormSchema {
		return {
			fields: [
				{
					name: `${prefix}Enabled`,
					label: '有効（無効にすると、この接続の Hub のタグは購読も収集もしません）',
					type: 'checkbox',
					default: true
				}
			]
		};
	}

	const HUB_CONNECTION_WIRE_FIELDS = schemaWireFields(
		HUB_EDIT,
		hubConnectionSchema(HUB_EDIT).fields
	);

	let editHubConnectionStore = $state(
		untrack(() => createFormStore(hubConnectionSchema(HUB_EDIT)))
	);

	/** Hub 接続の「有効」の保存（`saveConnection` と同じ取り違え対策）。 */
	async function saveHubConnection(): Promise<void> {
		if (!selectedConnection || savingConnection || deletingConnection) return;
		const pending: SaveGuardToken<typeof editHubConnectionStore> = {
			id: selectedConnection.id,
			store: editHubConnectionStore
		};
		const before = selectedConnection;
		savingConnection = true;
		try {
			const outcome = await runGuardedSave(
				pending,
				updatePlcConnection(
					selectedConnection.id,
					hubConnectionInput(
						selectedConnection,
						Boolean(editHubConnectionStore.values[`${HUB_EDIT}Enabled`])
					)
				),
				() => ({ id: selectedConnection?.id, store: editHubConnectionStore })
			);
			switch (outcome.kind) {
				case 'applied':
					toastStore.push('success', hubConnectionSavedMessage(before, outcome.entity));
					editHubConnectionStore.markClean();
					selectedConnection = outcome.entity;
					await reloadConnections();
					break;
				case 'stale-success':
					await reloadConnections();
					break;
				case 'error':
					applyServerErrors(
						HUB_EDIT,
						HUB_CONNECTION_WIRE_FIELDS,
						outcome.err,
						editHubConnectionStore
					);
					break;
				case 'stale-error':
					break;
			}
		} finally {
			savingConnection = false;
		}
	}

	/**
	 * #394 レビュー（追補2）: `deletingConnection`が真の間も編集フォーム
	 * （`BantoForm`の`submitting`）が素通しだと、削除の応答待ちの間に同じIDへ
	 * PUTを送れてしまう - `runGuardedSave`/`runGuardedDelete`の照合は「別の
	 * 編集対象への応答混入」を防ぐだけで、同じID・同じフォームのまま始まる
	 * 保存と削除の並走は防げない。開始条件に`savingConnection`も含めるのは
	 * 保存ボタンの二重発火（PUTの二重送信）も一緒に塞ぐため。
	 */
	async function saveConnection(): Promise<void> {
		if (!selectedConnection || savingConnection || deletingConnection) return;
		if (!editConnectionStore.validateAll()) return;
		const driver = driverFor(String(editConnectionStore.values[`${PLC_EDIT}Protocol`] ?? ''));
		if (!driver) return;
		editDriverErrors = validateDriverValues(driver.settingsSchema.fields, editDriverState.values);
		if (Object.keys(editDriverErrors).length > 0) return;
		const pending: SaveGuardToken<typeof editConnectionStore> = {
			id: selectedConnection.id,
			store: editConnectionStore
		};
		// #413: 保存前の行（切替が起きたかの判定用）。
		const before = selectedConnection;
		savingConnection = true;
		try {
			const outcome = await runGuardedSave(
				pending,
				updatePlcConnection(
					selectedConnection.id,
					toPlcConnectionInput(
						PLC_EDIT,
						editConnectionStore.values,
						buildDriverSettings(driver.settingsSchema.fields, editDriverState.values)
					)
				),
				() => ({ id: selectedConnection?.id, store: editConnectionStore })
			);
			switch (outcome.kind) {
				case 'applied':
					// #413: simulation を切り替えたなら「収集を再起動」を案内する。
					// 判定は**サーバーが返した行**で行う（失敗したら `error` 分岐に
					// 行き、ここは通らない - 画面は切り替わったように見せない）。
					toastStore.push('success', connectionSavedMessage(before, outcome.entity));
					editConnectionStore.markClean();
					editDriverState = driverStateFromSettings(
						driver.settingsSchema.fields,
						outcome.entity.settings
					);
					editDriverErrors = {};
					selectedConnection = outcome.entity;
					await reloadConnections();
					break;
				case 'stale-success':
					// 応答が返ってきた時点で既に別の接続を選び直している - 一覧の
					// 鮮度だけは保つが、今開いているフォームには適用しない。
					await reloadConnections();
					break;
				case 'error':
					applyConnectionErrors(PLC_EDIT, outcome.err, editConnectionStore, driver, false);
					break;
				case 'stale-error':
					// 古いエラーを今開いている別の接続のフォームには出さない
					// （トーストも出さない - ユーザーは既に別の行を見ている）。
					break;
			}
		} finally {
			savingConnection = false;
		}
	}

	/**
	 * #394 追補: 削除の`await`中に別の行を選び直すと、応答が返った時点で
	 * `selectedConnection = null`が無条件に走り、その行で編集中だった
	 * 未保存の入力が黙って消えていた（#378の「未保存の入力を黙って捨て
	 * ない」方針に反する）。保存側と同じ形で「削除中は選択・削除ボタンを
	 * 止める」+「応答時に対象IDを照合する」の両方を行う
	 * （`runGuardedDelete`のdoc comment参照）。
	 */
	async function handleDeleteConnection(): Promise<void> {
		if (!selectedConnection || savingConnection || deletingConnection) return;
		if (!window.confirm(`${selectedConnection.name} を削除しますか？`)) return;
		const pending: DeleteGuardToken = { id: selectedConnection.id };
		deletingConnection = true;
		try {
			const outcome = await runGuardedDelete(
				pending,
				deletePlcConnection(selectedConnection.id),
				() => selectedConnection?.id
			);
			switch (outcome.kind) {
				case 'applied':
					toastStore.push('success', '削除しました');
					selectedConnection = null;
					// #1179 PR4b: 消した接続先の「設定」から「すべて」へ移る。
					forceView(
						{ target: { type: 'all' }, tab: 'list' },
						{ kind: 'connection', id: pending.id }
					);
					await reloadConnections();
					break;
				case 'stale-success':
					// 既に別の接続を選び直している - その選択（未保存の入力）を
					// 消さない。一覧の鮮度だけは保つ。
					await reloadConnections();
					break;
				case 'error':
					toastStore.push('error', errorMessage(outcome.err));
					break;
				case 'stale-error':
					// 古いエラーを今開いている別の接続に出さない（保存側の
					// stale-errorと同じ考え方）。
					break;
			}
		} finally {
			deletingConnection = false;
		}
	}

	// --- 収集グループ ---------------------------------------------------------

	const GROUP_CREATE = 'groupCreate';
	const GROUP_EDIT = 'groupEdit';

	const periodOptions = $derived(
		ALLOWED_PERIOD_MS.map((ms) => ({ value: ms, label: formatPeriodMs(ms) }))
	);
	const connectionOptions = $derived(
		connectionRows.map((c) => ({ value: c.id, label: connectionOptionLabel(c) }))
	);

	function groupSchema(prefix: string): FormSchema {
		return {
			fields: [
				{ name: `${prefix}Name`, label: '名前', type: 'text', required: true, min: 1, max: 100 },
				{
					name: `${prefix}PlcConnectionId`,
					label: '接続',
					type: 'select',
					required: true,
					options: connectionOptions
				},
				{
					name: `${prefix}PeriodMs`,
					label: '収集周期',
					type: 'select',
					required: true,
					default: 1000,
					options: periodOptions
				},
				{ name: `${prefix}Enabled`, label: '有効', type: 'checkbox', default: true }
			]
		};
	}

	function toCollectionGroupInput(
		prefix: string,
		values: Record<string, unknown>
	): CollectionGroupInput {
		const plcConnectionId = values[`${prefix}PlcConnectionId`];
		const periodMs = values[`${prefix}PeriodMs`];
		return {
			name: String(values[`${prefix}Name`] ?? ''),
			plcConnectionId: typeof plcConnectionId === 'number' ? plcConnectionId : 0,
			periodMs: typeof periodMs === 'number' ? periodMs : 1000,
			enabled: Boolean(values[`${prefix}Enabled`])
		};
	}

	function groupFormValues(prefix: string, group: CollectionGroup): Record<string, unknown> {
		return {
			[`${prefix}Name`]: group.name,
			[`${prefix}PlcConnectionId`]: group.plcConnectionId,
			[`${prefix}PeriodMs`]: group.periodMs,
			[`${prefix}Enabled`]: group.enabled
		};
	}

	// `CONNECTION_WIRE_FIELDS`と同じ理由。`groupSchema`のフィールド名は
	// `connectionOptions`（$derived）の中身に関わらず固定なので、ここで
	// 一度だけ計算してよい。
	const GROUP_WIRE_FIELDS = schemaWireFields(GROUP_CREATE, groupSchema(GROUP_CREATE).fields);

	// `connections` と同じ3状態（未読込 / 失敗 / 読めた）。
	let groups: CollectionGroup[] | null = $state(null);
	let groupsError: string | null = $state(null);
	let groupsLoading = $state(false);
	const groupsState: ListLoadState<CollectionGroup> = $derived({
		items: groups,
		error: groupsError
	});
	const groupRows = $derived(listRows(groupsState));
	const groupsView = $derived(listSectionView(groupsState));
	const groupsRetry = $derived(showsRetry(groupsState));
	// 収集グループの作成フォームは PLC接続 の一覧に依存する（`<select>` の
	// 候補）。「先にPLC接続を1件以上作成してください」を出してよいのは
	// **読めた結果0件**のときだけ（#394レビュー P1-3）。
	const groupCreateGate = $derived(createFormGate(connectionsState));

	/** `connectionsGeneration` と同じ（レビュー P2-C）。 */
	let groupsGeneration = 0;

	async function reloadGroups(): Promise<void> {
		groupsGeneration += 1;
		const generation = groupsGeneration;
		groupsLoading = true;
		groupsError = null;
		const outcome = await runGuardedListLoad(
			generation,
			listCollectionGroups(),
			() => groupsGeneration
		);
		switch (outcome.kind) {
			case 'applied':
				groups = outcome.items;
				// #414 段階2: どの一覧が変わっても「開始時に外される設定」の判定は変わる。
				void reloadConfigExclusions();
				break;
			case 'error':
				groupsError = errorMessage(outcome.err);
				toastStore.push('error', groupsError);
				break;
			case 'stale':
				return;
		}
		groupsLoading = false;
	}

	// `untrack`: このPRのフォームstoreはoptionsの中身までは使わない（options
	// は別途 `<BantoForm schema={groupSchema(...)}>` へ渡す方が常に最新を反映
	// するので、こちらは初期スナップショットで十分 - Svelteの
	// `state_referenced_locally`警告を意図どおり黙らせる）。
	let createGroupStore = $state(untrack(() => createFormStore(groupSchema(GROUP_CREATE))));
	let creatingGroup = $state(false);

	async function handleCreateGroup(values: Record<string, unknown>): Promise<void> {
		creatingGroup = true;
		try {
			const created = await createCollectionGroup(toCollectionGroupInput(GROUP_CREATE, values));
			toastStore.push('success', '作成しました');
			createGroupStore = createFormStore(groupSchema(GROUP_CREATE));
			await reloadGroups();
			// #1179 PR4b: 作った収集グループをツリーで選ぶ（右の作成フォームは閉じる）。
			selectCreated('createGroup', { type: 'group', id: created.id });
		} catch (err) {
			applyServerErrors(GROUP_CREATE, GROUP_WIRE_FIELDS, err, createGroupStore);
		} finally {
			creatingGroup = false;
		}
	}

	let selectedGroup: CollectionGroup | null = $state(null);
	let editGroupStore = $state(untrack(() => createFormStore(groupSchema(GROUP_EDIT))));
	let savingGroup = $state(false);
	let deletingGroup = $state(false);

	/** #1179 PR4b: 「設定」タブの収集グループのフォームを作り直す（`loadConnectionSettings` と同じ）。 */
	function loadGroupSettings(group: CollectionGroup): void {
		selectedGroup = group;
		editGroupStore = createFormStore(groupSchema(GROUP_EDIT), groupFormValues(GROUP_EDIT, group));
	}

	/** #394 レビュー（追補2）: 削除中も保存を素通しさせない（理由は `saveConnection` 参照）。 */
	async function saveGroup(): Promise<void> {
		if (!selectedGroup || savingGroup || deletingGroup) return;
		if (!editGroupStore.validateAll()) return;
		const pending: SaveGuardToken<typeof editGroupStore> = {
			id: selectedGroup.id,
			store: editGroupStore
		};
		savingGroup = true;
		try {
			const outcome = await runGuardedSave(
				pending,
				updateCollectionGroup(
					selectedGroup.id,
					toCollectionGroupInput(GROUP_EDIT, editGroupStore.values)
				),
				() => ({ id: selectedGroup?.id, store: editGroupStore })
			);
			switch (outcome.kind) {
				case 'applied':
					toastStore.push('success', '更新しました');
					editGroupStore.markClean();
					selectedGroup = outcome.entity;
					await reloadGroups();
					break;
				case 'stale-success':
					await reloadGroups();
					break;
				case 'error':
					applyServerErrors(GROUP_EDIT, GROUP_WIRE_FIELDS, outcome.err, editGroupStore);
					break;
				case 'stale-error':
					break;
			}
		} finally {
			savingGroup = false;
		}
	}

	/** #394 追補: 削除中の応答取り違え対策（理由は `handleDeleteConnection` 参照）。 */
	async function handleDeleteGroup(): Promise<void> {
		if (!selectedGroup || savingGroup || deletingGroup) return;
		if (!window.confirm(`${selectedGroup.name} を削除しますか？`)) return;
		const pending: DeleteGuardToken = { id: selectedGroup.id };
		deletingGroup = true;
		try {
			const outcome = await runGuardedDelete(
				pending,
				deleteCollectionGroup(selectedGroup.id),
				() => selectedGroup?.id
			);
			switch (outcome.kind) {
				case 'applied': {
					toastStore.push('success', '削除しました');
					// #1179 PR4b: 消したグループの「設定」から、親の接続先へ移る。
					const parent = selectedGroup?.plcConnectionId;
					selectedGroup = null;
					forceView(
						{
							target: parent === undefined ? { type: 'all' } : { type: 'connection', id: parent },
							tab: 'list'
						},
						{ kind: 'group', id: pending.id }
					);
					await reloadGroups();
					break;
				}
				case 'stale-success':
					await reloadGroups();
					break;
				case 'error':
					toastStore.push('error', errorMessage(outcome.err));
					break;
				case 'stale-error':
					break;
			}
		} finally {
			deletingGroup = false;
		}
	}

	// --- タグ -----------------------------------------------------------------

	const TAG_CREATE = 'tagCreate';
	const TAG_EDIT = 'tagEdit';

	const groupOptions = $derived(groupRows.map((g) => ({ value: g.id, label: g.name })));

	/**
	 * `hub` = Hub 経由のタグ（#383 段階3 P4）。アドレスは Hub のタグ名（外部名）で、
	 * **スケーリングの欄を出さない**（Hub は工学値を送るので、ここで換算すると二重に
	 * なる。サーバーの `banto-tags` も断る）。作成では `buildTagInput` が
	 * スケーリングを `null` で送り、編集では編集前のタグの値をそのまま送る。
	 */
	function tagSchema(prefix: string, hub = false): FormSchema {
		const scalingFields: FormSchema['fields'] = hub
			? []
			: [
					{ name: `${prefix}RawLo`, label: 'スケーリング: 生値 下限', type: 'number' },
					{ name: `${prefix}RawHi`, label: 'スケーリング: 生値 上限', type: 'number' },
					{ name: `${prefix}EngLo`, label: 'スケーリング: 工学値 下限', type: 'number' },
					{ name: `${prefix}EngHi`, label: 'スケーリング: 工学値 上限', type: 'number' }
				];
		return {
			fields: [
				{ name: `${prefix}Name`, label: '名前', type: 'text', required: true, min: 1, max: 100 },
				{
					name: `${prefix}CollectionGroupId`,
					label: '収集グループ',
					type: 'select',
					required: true,
					options: groupOptions
				},
				{
					name: `${prefix}Address`,
					label: hub ? 'Hub のタグ名（外部名）' : 'デバイスアドレス',
					type: 'text',
					required: true,
					placeholder: hub ? '接続名.グループ名.タグ名' : 'D3000 / 40001 など'
				},
				{
					name: `${prefix}DataType`,
					label: 'データ型',
					type: 'select',
					required: true,
					default: 'i16',
					options: tagDataTypeOptions(hub)
				},
				...scalingFields,
				{ name: `${prefix}Unit`, label: '単位', type: 'text' },
				// min/max は banto_tags::tag の MIN_DECIMALS/MAX_DECIMALS と一致
				// させる（クライアント側とサーバー側の検証範囲がずれると、
				// クライアントを通った値がサーバーで人間可読エラーとして
				// 跳ね返るだけの手戻りになる）。
				{ name: `${prefix}Decimals`, label: '小数桁', type: 'number', min: 0, max: 6, default: 0 },
				// しきい値はここに無い（#532: タグ定義の属性ではなく、記録計の側の設定。
				// 編集ペインの下の「しきい値」のフォームで別に保存する）。
				{ name: `${prefix}Enabled`, label: '有効', type: 'checkbox', default: true }
			]
		};
	}

	// `CONNECTION_WIRE_FIELDS`と同じ理由。
	const TAG_WIRE_FIELDS = schemaWireFields(TAG_CREATE, tagSchema(TAG_CREATE).fields);

	// `connections` と同じ3状態（未読込 / 失敗 / 読めた）。
	let tags: Tag[] | null = $state(null);
	let tagsError: string | null = $state(null);
	let tagsLoading = $state(false);
	const tagsState: ListLoadState<Tag> = $derived({ items: tags, error: tagsError });
	const tagRows = $derived(listRows(tagsState));
	const tagsView = $derived(listSectionView(tagsState));
	const tagsRetry = $derived(showsRetry(tagsState));

	/** `connectionsGeneration` と同じ（レビュー P2-C）。 */
	let tagsGeneration = 0;

	async function reloadTags(): Promise<void> {
		tagsGeneration += 1;
		const generation = tagsGeneration;
		tagsLoading = true;
		tagsError = null;
		const outcome = await runGuardedListLoad(generation, listTags(), () => tagsGeneration);
		switch (outcome.kind) {
			case 'applied':
				tags = outcome.items;
				// #414 段階2: どの一覧が変わっても「開始時に外される設定」の判定は変わる。
				void reloadConfigExclusions();
				break;
			case 'error':
				tagsError = errorMessage(outcome.err);
				toastStore.push('error', tagsError);
				break;
			case 'stale':
				return;
		}
		tagsLoading = false;
	}

	let createTagStore = $state(untrack(() => createFormStore(tagSchema(TAG_CREATE))));
	let creatingTag = $state(false);

	/**
	 * 作成フォームで選んでいる収集グループが Hub 接続の下か。そうなら編集と同じ
	 * Hub 用の欄（外部名・スケーリング無し）に切り替える。
	 */
	const createTagIsHub = $derived(
		isHubGroup(Number(createTagStore.values[`${TAG_CREATE}CollectionGroupId`]))
	);

	async function handleCreateTag(values: Record<string, unknown>): Promise<void> {
		creatingTag = true;
		try {
			await createTag(
				buildTagInput(TAG_CREATE, values, undefined, createTagIsHub ? 'none' : 'form')
			);
			toastStore.push('success', '作成しました');
			createTagStore = createFormStore(tagSchema(TAG_CREATE));
			// #1179 PR4b: 保存に成功したら右ペインを閉じ、一覧を読み直す。
			if (rightPane === 'createTag') closeRightPane();
			await reloadTags();
		} catch (err) {
			applyServerErrors(TAG_CREATE, TAG_WIRE_FIELDS, err, createTagStore);
		} finally {
			creatingTag = false;
		}
	}

	function groupName(id: number): string {
		return groupRows.find((g) => g.id === id)?.name ?? `#${id}`;
	}

	const tagColumns: GridColumn<Tag>[] = [
		{ id: 'id', header: 'ID', accessor: 'id', width: 60, align: 'right' },
		{
			id: 'name',
			header: '名前',
			accessor: 'name',
			width: 160,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'collectionGroupId',
			header: '収集グループ',
			accessor: (row) => groupName(row.collectionGroupId),
			width: 140
		},
		{ id: 'address', header: 'アドレス', accessor: 'address', width: 110 },
		{ id: 'dataType', header: 'データ型', accessor: 'dataType', width: 90 },
		{ id: 'unit', header: '単位', accessor: 'unit', width: 80 },
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		}
	];

	let selectedTag: Tag | null = $state(null);
	let editTagStore = $state(untrack(() => createFormStore(tagSchema(TAG_EDIT))));
	let savingTag = $state(false);
	let deletingTag = $state(false);

	/** #383 段階3 P4: 収集グループが Hub 接続の下にあるか。 */
	function isHubGroup(groupId: number): boolean {
		const group = groupRows.find((g) => g.id === groupId);
		if (!group) return false;
		return connectionRows.find((c) => c.id === group.plcConnectionId)?.protocol === 'hub';
	}

	/**
	 * 同じ行を選んだとき、最新の版で作り直すか。タグの版が違う（版の衝突の後に一覧を
	 * 読み直した）か、しきい値の保存が版の衝突で断られたあとのときだけ。
	 */
	function shouldReselectTag(
		current: Tag | null,
		clicked: Tag,
		thresholdsConflicted: boolean
	): boolean {
		if (current === null || current.id !== clicked.id) return false;
		return thresholdsConflicted || current.revision !== clicked.revision;
	}

	/** 編集中のタグが Hub 経由か（スケーリングの欄を出さない）。選んだ時点で決める。 */
	let selectedTagIsHub = $state(false);

	/**
	 * 一覧の行を選ぶと、右ペインに記録項目の詳細を出す。#391 レビュー A / #394
	 * 追補: 保存中・削除中は選択行を切り替えさせない。#1179 PR4b: 右ペインの
	 * フォームが未保存なら確かめる（キャンセルなら元のまま）。
	 */
	function selectTag(tag: Tag): void {
		if (!canWrite || rightPaneBusy()) return;
		if (rightPane === 'tag' && selectedTag?.id === tag.id) {
			// 同じ行: 版が同じなら何もしない。違えば（版の衝突の後に一覧を読み直した）
			// 選び直しとして最新の版で作り直す。
			if (!shouldReselectTag(selectedTag, tag, thresholdsConflict)) return;
		}
		if (!confirmDiscardRightPane()) return;
		selectedTag = tag;
		openRightPane('tag');
		selectedTagIsHub = isHubGroup(tag.collectionGroupId);
		editTagStore = createFormStore(
			tagSchema(TAG_EDIT, selectedTagIsHub),
			tagFormValues(TAG_EDIT, tag)
		);
		void loadThresholds(tag.id);
	}

	// --- #383 段階3 P4: Hub から追加 -------------------------------------------

	let hubDialogOpen = $state(false);

	async function afterHubRegistration(): Promise<void> {
		await Promise.all([reloadConnections(), reloadGroups(), reloadTags()]);
	}

	// --- #532: 記録計の側のしきい値（タグとは別の保存） -------------------------
	//
	// しきい値はタグ定義の属性ではなく、記録計の側のタグごとの設定。タグの編集
	// ペインの下に**別のフォーム**（別の「しきい値を保存」・別の版）として置く。
	// タグのフォームに混ぜて 1 回の保存で 2 つを書くと、片方だけ通ったときの
	// 状態（タグは保存されたがしきい値は断られた）を画面が説明しきれないため。
	// 新しいタグには、作成した後に行を選んで設定する（既定は設定なし）。
	//
	// - 読み込みは行を選んだとき。応答が返った時点で別の行を選んでいたら捨てる
	//   （`thresholdsGeneration` と選択中のタグ ID の両方で照合）。
	// - 読めなかったときはフォームを出さず、理由と「再読み込み」を出す（空の
	//   フォームを出すと「設定なし」と区別できず、保存で既存の値を消してしまう）。
	// - 保存の取り違え対策はタグと同じ `runGuardedSave`。版の食い違いは
	//   トーストで案内し（`expectedRevision` はフォームの欄ではない）、入力は
	//   消さない。行を選び直すと最新の版で編集できる。

	const TAG_THR = 'tagThr';

	function thresholdSchema(prefix: string): FormSchema {
		return {
			fields: [
				// 空欄 = 設定なし。大小関係（LL <= L <= H <= HH）はサーバーが検証し、
				// 崩れた側の欄のすぐ下に出る。
				{ name: `${prefix}ThresholdLl`, label: 'しきい値 LL（下下限）', type: 'number' },
				{ name: `${prefix}ThresholdL`, label: 'しきい値 L（下限）', type: 'number' },
				{ name: `${prefix}ThresholdH`, label: 'しきい値 H（上限）', type: 'number' },
				{ name: `${prefix}ThresholdHh`, label: 'しきい値 HH（上上限）', type: 'number' }
			]
		};
	}

	let selectedThresholds: TagThresholds | null = $state(null);
	let thresholdsError: string | null = $state(null);
	let thresholdsGeneration = 0;
	let thresholdsConflict = false;
	let editThresholdsStore = $state(untrack(() => createFormStore(thresholdSchema(TAG_THR))));
	let savingThresholds = $state(false);

	async function loadThresholds(tagId: number): Promise<void> {
		thresholdsGeneration += 1;
		const generation = thresholdsGeneration;
		selectedThresholds = null;
		thresholdsError = null;
		thresholdsConflict = false;
		try {
			const loaded = await getTagThresholds(tagId);
			if (generation !== thresholdsGeneration || selectedTag?.id !== tagId) return;
			selectedThresholds = loaded;
			editThresholdsStore = createFormStore(
				thresholdSchema(TAG_THR),
				thresholdFormValues(TAG_THR, loaded)
			);
		} catch (err) {
			if (generation !== thresholdsGeneration || selectedTag?.id !== tagId) return;
			thresholdsError = errorMessage(err);
		}
	}

	// しきい値の版（`selectedThresholds.revision`）はタグの版とは別。しきい値だけが
	// 版の衝突で断られたら `thresholdsConflict` を立て、同じ行の選び直しでも
	// `selectTag` が読み直す（`loadThresholds` が下ろす）。
	async function saveThresholds(): Promise<void> {
		if (!selectedTag || !selectedThresholds || savingThresholds || savingTag || deletingTag) {
			return;
		}
		if (!editThresholdsStore.validateAll()) return;
		const pending: SaveGuardToken<typeof editThresholdsStore> = {
			id: selectedTag.id,
			store: editThresholdsStore
		};
		savingThresholds = true;
		try {
			const outcome = await runGuardedSave(
				pending,
				updateTagThresholds(
					selectedTag.id,
					buildThresholdsInput(TAG_THR, editThresholdsStore.values, selectedThresholds.revision)
				),
				() => ({ id: selectedTag?.id, store: editThresholdsStore })
			);
			switch (outcome.kind) {
				case 'applied':
					toastStore.push('success', THRESHOLDS_SAVED_MESSAGE);
					editThresholdsStore.markClean();
					selectedThresholds = outcome.entity;
					break;
				case 'error':
					applyServerErrors(TAG_THR, THRESHOLD_WIRE_FIELDS, outcome.err, editThresholdsStore);
					if (isRevisionConflict(outcome.err)) thresholdsConflict = true;
					break;
				case 'stale-success':
				case 'stale-error':
					break;
			}
		} finally {
			savingThresholds = false;
		}
	}

	/** #394 レビュー（追補2）: 削除中も保存を素通しさせない（理由は `saveConnection` 参照）。 */
	async function saveTag(): Promise<void> {
		if (!selectedTag || savingTag || deletingTag || savingThresholds) return;
		if (!editTagStore.validateAll()) return;
		const pending: SaveGuardToken<typeof editTagStore> = {
			id: selectedTag.id,
			store: editTagStore
		};
		savingTag = true;
		try {
			const outcome = await runGuardedSave(
				pending,
				updateTag(
					selectedTag.id,
					buildTagInput(
						TAG_EDIT,
						editTagStore.values,
						selectedTag.revision,
						// Hub タグは画面に欄が無いので、編集前のタグが持つスケーリングをそのまま送る。
						selectedTagIsHub
							? {
									rawLo: selectedTag.rawLo,
									rawHi: selectedTag.rawHi,
									engLo: selectedTag.engLo,
									engHi: selectedTag.engHi
								}
							: 'form'
					)
				),
				() => ({ id: selectedTag?.id, store: editTagStore })
			);
			switch (outcome.kind) {
				case 'applied':
					toastStore.push('success', '更新しました');
					editTagStore.markClean();
					selectedTag = outcome.entity;
					await reloadTags();
					break;
				case 'stale-success':
					await reloadTags();
					break;
				case 'error':
					applyServerErrors(TAG_EDIT, TAG_WIRE_FIELDS, outcome.err, editTagStore);
					// #525: 他者が先に更新していた。案内はトーストに出ている（`expectedRevision`
					// はフォームの項目ではない）。一覧を最新にして、入力中の値は消さない
					// （再読込後に行を選び直すと最新の版で編集できる）。
					if (isRevisionConflict(outcome.err)) await reloadTags();
					break;
				case 'stale-error':
					break;
			}
		} finally {
			savingTag = false;
		}
	}

	/** #394 追補: 削除中の応答取り違え対策（理由は `handleDeleteConnection` 参照）。 */
	async function handleDeleteTag(): Promise<void> {
		if (!selectedTag || savingTag || deletingTag || savingThresholds) return;
		if (!window.confirm(`${selectedTag.name} を削除しますか？`)) return;
		const pending: DeleteGuardToken = { id: selectedTag.id };
		deletingTag = true;
		try {
			const outcome = await runGuardedDelete(
				pending,
				deleteTag(selectedTag.id),
				() => selectedTag?.id
			);
			switch (outcome.kind) {
				case 'applied':
					toastStore.push('success', '削除しました');
					closeRightPane();
					await reloadTags();
					break;
				case 'stale-success':
					await reloadTags();
					break;
				case 'error':
					toastStore.push('error', errorMessage(outcome.err));
					break;
				case 'stale-error':
					break;
			}
		} finally {
			deletingTag = false;
		}
	}

	// --- #1179 PR4b: 3 ペインの状態（右ペイン・中央のタブ・URL） -----------------
	//
	// docs/chronogazer/chronogazer-ui-improvement-proposal.md §2.1（2026-11-18）。HUB のタグ
	// ページ（`apps/banto-hub/src/routes/(app)/tags/+page.svelte`、#1179 PR3a）の
	// 作法を写す:
	// - **画面の操作（ツリー・タブ）は状態を先に同期で変え、URL は後から追う**
	//   （`requestView`）。BantoTree は `selection` の制御で「呼び出しの直後の値」を
	//   正とするので、URL から状態を決めるとキー操作のたびに元の行へ引き戻される。
	// - **URL から状態を決めるのは、戻る・進む・再読み込み・リンク**（下の
	//   `$effect`）。ページ内の `goto` が終わるまではその `$effect` を止める
	//   （`viewNavsInFlight`）。
	// - **未保存の確認**は `confirmViewChange`（対象・タブの切り替え）と
	//   `confirmDiscardRightPane`（右ペインの中身の切り替え・閉じる）。どちらも
	//   キャンセルなら何も変えない。確かめて進んだら、捨てたフォームは閉じる・
	//   作り直す（`discardForView`）。
	// - **ページの中の移動では汎用の離脱の確認（`guardUnsavedChanges`）を出さない**。
	//   クエリだけが変わる移動は、guard に `isSamePage: isSamePath`（banto v7.6.0）を渡して
	//   同じページの移動とみなす。確認はページ側（`confirmViewChange`）が済ませる。
	//   戻る・進むは `handleBeforeNavigate`（`beforeNavigate`）で確かめ、取り消すなら
	//   `nav.cancel()`。
	// - 対象・タブを変えると右ペインは閉じる（右ペインの中身は、前の対象の一覧の
	//   行か、前の対象への追加だったため）。**例外: 削除に伴う移動（`forceView`）は
	//   右ペインを閉じない**（無関係な作成フォームの下書きを守る。作成フォームの
	//   親の選択欄が消したものを指していたら、その欄だけ未選択に戻す）。

	/** 右ペインの中身（どれか 1 つ）。`null` = 閉じている。 */
	type RightPaneKind = 'tag' | 'createTag' | 'createConnection' | 'createGroup';
	let rightPane: RightPaneKind | null = $state(null);

	/** 未保存のフォームを捨てて切り替えてよいかの確認文。 */
	const DISCARD_CONFIRM = '保存していない変更があります。変更を破棄して切り替えますか？';

	function rightPaneDirty(): boolean {
		switch (rightPane) {
			case 'tag':
				return (
					editTagStore.isDirty ||
					// #532: しきい値のフォームは読めたときだけ出るので、そのときだけ数える。
					(selectedThresholds !== null && editThresholdsStore.isDirty)
				);
			case 'createTag':
				return createTagStore.isDirty;
			case 'createConnection':
				return (
					createConnectionStore.isDirty || Object.values(createDriverState.touched).some(Boolean)
				);
			case 'createGroup':
				return createGroupStore.isDirty;
			default:
				return false;
		}
	}

	function rightPaneBusy(): boolean {
		switch (rightPane) {
			case 'tag':
				return savingTag || deletingTag || savingThresholds;
			case 'createTag':
				return creatingTag;
			case 'createConnection':
				return creatingConnection;
			case 'createGroup':
				return creatingGroup;
			default:
				return false;
		}
	}

	/** 「設定」タブのフォームが未保存か（タブが「設定」のときだけ数える）。 */
	function settingsDirty(): boolean {
		if (activeTab !== 'settings') return false;
		if (target.type === 'connection' && selectedConnection !== null) {
			return selectedConnectionIsHub
				? editHubConnectionStore.isDirty
				: editConnectionStore.isDirty || Object.values(editDriverState.touched).some(Boolean);
		}
		if (target.type === 'group' && selectedGroup !== null) return editGroupStore.isDirty;
		return false;
	}

	function settingsBusy(): boolean {
		return savingConnection || deletingConnection || savingGroup || deletingGroup;
	}

	/** 右ペインの中身を捨ててよいか（保存中は不可、未保存なら確かめる）。 */
	function confirmDiscardRightPane(): boolean {
		if (rightPaneBusy()) return false;
		return !rightPaneDirty() || window.confirm(DISCARD_CONFIRM);
	}

	/** 狭幅で右ペイン（ドロワー）が開いているか（`ThreePaneLayout` の `rightDrawerOpen`）。 */
	let rightDrawerOpen = $state(false);
	/** 狭幅で左ツリー（ドロワー）が開いているか（`ThreePaneLayout` の `leftDrawerOpen`）。 */
	let treeOpen = $state(false);
	/** 枠（`ThreePaneLayout`）が狭幅か（部品の `bind:narrow`、出力専用）。 */
	let layoutNarrow = $state(false);

	function openRightPane(kind: RightPaneKind): void {
		rightPane = kind;
		if (layoutNarrow) {
			treeOpen = false;
			rightDrawerOpen = true;
		}
	}

	function closeRightPane(): void {
		rightPane = null;
		rightDrawerOpen = false;
		selectedTag = null;
		// 読み込み中のしきい値の応答は捨てる（`loadThresholds` が世代で照合する）。
		thresholdsGeneration += 1;
		selectedThresholds = null;
		thresholdsError = null;
	}

	function requestCloseRightPane(): void {
		if (!confirmDiscardRightPane()) return;
		closeRightPane();
	}

	/** 「接続先を追加」（PLC 直結。Hub 経由の接続は「Hub から追加」が作る）。 */
	function openCreateConnection(): void {
		if (!canWrite || rightPane === 'createConnection') return;
		if (!confirmDiscardRightPane()) return;
		closeRightPane();
		createConnectionStore = createFormStore(connectionSchema(PLC_CREATE));
		createDriverState = defaultDriverState([]);
		createDriverId = '';
		createDriverErrors = {};
		openRightPane('createConnection');
	}

	/** 選んでいる接続先（グループを選んでいればその接続先）。 */
	const targetConnectionId = $derived.by((): number | null => {
		if (target.type === 'connection') return target.id;
		if (target.type === 'group') {
			const id = target.id;
			return groupRows.find((g) => g.id === id)?.plcConnectionId ?? null;
		}
		return null;
	});

	/** 「収集グループを追加」。接続は選択中の接続先（グループの接続先）を初期値にする。 */
	function openCreateGroup(): void {
		if (!canWrite || rightPane === 'createGroup') return;
		if (!confirmDiscardRightPane()) return;
		closeRightPane();
		const connectionId = targetConnectionId;
		createGroupStore = createFormStore(
			groupSchema(GROUP_CREATE),
			connectionId === null ? {} : { [`${GROUP_CREATE}PlcConnectionId`]: connectionId }
		);
		openRightPane('createGroup');
	}

	/** 記録項目の登録先（収集グループを選んでいるときだけ。HUB の 2026-11-18 オーナー決定と同じ）。 */
	const registrationGroup = $derived.by((): CollectionGroup | null => {
		if (target.type !== 'group') return null;
		const id = target.id;
		return groupRows.find((g) => g.id === id) ?? null;
	});

	/** 「記録項目を追加」。収集グループは選択中のグループを初期値にする。 */
	function openCreateTag(): void {
		const group = registrationGroup;
		if (!canWrite || group === null || rightPane === 'createTag') return;
		if (!confirmDiscardRightPane()) return;
		closeRightPane();
		createTagStore = createFormStore(tagSchema(TAG_CREATE, isHubGroup(group.id)), {
			[`${TAG_CREATE}CollectionGroupId`]: group.id
		});
		openRightPane('createTag');
	}

	/** 右ペインの見出し（と `ThreePaneLayout` の右ペインの名前）。 */
	const rightPaneTitle = $derived.by((): string => {
		switch (rightPane) {
			case 'tag':
				return selectedTag?.name ?? '記録項目';
			case 'createTag':
				return '記録項目を追加';
			case 'createConnection':
				return '接続先を追加';
			case 'createGroup':
				return '収集グループを追加';
			default:
				return '詳細';
		}
	});

	/** ツリーで選んでいる対象（URL の `target`）。 */
	let target: RecordTarget = $state({ type: 'all' });
	/** 中央のタブ（URL の `tab`）。「すべて」と閲覧者は常に `'list'`。 */
	let activeTab: RecordsTab = $state('list');
	/** 接続先と収集グループの一覧を読み終えたか（URL の対象の存在を判定してよいか）。 */
	const registryLoaded = $derived(connections !== null && groups !== null);

	function currentView(): RecordsView {
		return { target, tab: activeTab };
	}

	function applyView(view: RecordsView): void {
		if (!sameRecordTarget(target, view.target)) target = view.target;
		if (activeTab !== view.tab) activeTab = view.tab;
	}

	/**
	 * 対象・タブの切り替えで捨てるもの: 右ペインを閉じ、「設定」タブのフォームは
	 * 次に出すときに保存済みの値から作り直す（`selectedConnection`/`selectedGroup`
	 * を外すと、下の `$effect` が今の対象で作り直す）。
	 */
	function discardForView(): void {
		closeRightPane();
		selectedConnection = null;
		selectedGroup = null;
	}

	/**
	 * 画面の状態を変えてよいか確かめる（キャンセルなら `false`。そのときは何も
	 * 変えない）。保存中は変えさせない（#391 レビュー A / #394 追補 - 応答の
	 * 取り違え対策は `runGuardedSave` 側にもある）。
	 */
	function confirmViewChange(next: RecordsView): boolean {
		const current = currentView();
		if (sameRecordsView(current, next)) return true;
		if (rightPaneBusy() || settingsBusy()) return false;
		if ((rightPaneDirty() || settingsDirty()) && !window.confirm(DISCARD_CONFIRM)) return false;
		discardForView();
		return true;
	}

	/** ページ内の `goto`（`requestView`・URL の正規化）の行き先。`beforeNavigate` は確認を飛ばす。 */
	const viewNavHrefs = new Set<string>();
	/** 進行中のページ内の `goto` の数。0 になったら URL から状態を決め直す。 */
	let viewNavsInFlight = $state(0);
	/**
	 * いまの履歴の項目が、ツリーの上下キーなどの移動で積んだものか。続けてキーで
	 * 動かす間は同じ項目を置き換える（HUB と同じ「1 操作 = 1 歩」。キーの移動は
	 * まとめて 1 歩）。
	 */
	let viewEntryFromKeyNavigation = false;

	/** 画面の状態を `next` にする。未保存の確認でキャンセルされたら `false`。 */
	function requestView(next: RecordsView, options: { keyNavigation?: boolean } = {}): boolean {
		if (sameRecordsView(currentView(), next)) return true;
		if (!confirmViewChange(next)) return false;
		const keyNavigation = options.keyNavigation === true;
		const replace = keyNavigation && viewEntryFromKeyNavigation;
		viewEntryFromKeyNavigation = keyNavigation;
		applyView(next);
		void navigateToView(next, { replace });
		return true;
	}

	/**
	 * 作成に成功した接続先・収集グループをツリーで選ぶ（右の作成フォームは閉じる）。
	 * 「設定」タブに別の対象の未保存の入力があれば、ふつうの切り替えと同じく確かめる
	 * （キャンセルなら今の対象のまま。作ったものはツリーに出ている）。
	 */
	function selectCreated(kind: RightPaneKind, created: RecordTarget): void {
		if (rightPane === kind) closeRightPane();
		requestView({ target: created, tab: 'list' });
	}

	/** 削除した接続先・グループ（作成フォームの親の選択欄が指していたら未選択に戻す）。 */
	type DeletedParent = { kind: 'connection' | 'group'; id: number };

	function clearDeletedParent(deleted: DeletedParent): void {
		const points = (store: { values: Record<string, unknown> }, name: string): boolean =>
			store.values[name] !== '' && Number(store.values[name]) === deleted.id;
		if (deleted.kind === 'connection') {
			const name = `${GROUP_CREATE}PlcConnectionId`;
			if (points(createGroupStore, name)) createGroupStore.setValue(name, '');
		} else {
			const name = `${TAG_CREATE}CollectionGroupId`;
			if (points(createTagStore, name)) createTagStore.setValue(name, '');
		}
	}

	/**
	 * 確かめずに移る（削除の成功の後）。「設定」タブ側の `selectedConnection`/
	 * `selectedGroup` は捨てるが、右ペインは残す（上記のとおり）。
	 */
	function forceView(next: RecordsView, deleted: DeletedParent): void {
		viewEntryFromKeyNavigation = false;
		// 削除に伴う移動は、ふつうの切り替え（`requestView`）と違い**右ペインを閉じない**。
		// 右ペインには消した対象と無関係な作成フォームが開いていることがあり（削除の
		// 応答待ちの間に開いたものも）、その入力を確認なしに捨てないため。
		selectedConnection = null;
		selectedGroup = null;
		clearDeletedParent(deleted);
		if (rightPane === 'tag' && selectedTag && tags && !tags.some((t) => t.id === selectedTag?.id)) {
			closeRightPane();
		}
		applyView(next);
		void navigateToView(next, { replace: false });
	}

	async function navigateToView(view: RecordsView, options: { replace: boolean }): Promise<void> {
		const url = new URL(page.url.href);
		url.search = recordsViewSearch(view);
		// 先の `goto` がまだ終わっていない間は `page.url` が古いので、同じに見えても省かない。
		if (url.href === page.url.href && viewNavsInFlight === 0) return;
		const href = url.href;
		viewNavHrefs.add(href);
		viewNavsInFlight += 1;
		try {
			// `reset: false`: フォーカスとスクロールを動かさない（同じページの中の切り替え）。
			await goto(url, { replace: options.replace, reset: false });
		} catch {
			// 失敗しても、下の `$effect` がその時点の URL へ状態を戻す。
		} finally {
			viewNavHrefs.delete(href);
			viewNavsInFlight -= 1;
		}
	}

	/**
	 * URL から画面の状態を決める（戻る・進む・再読み込み・リンク、とページ内の
	 * `goto` が取り消された場合）。一覧を読み終えたら、読めない値・一覧に無い id は
	 * 「すべて」へ倒し、URL も正規化した形へ置き換える（`replace`）。案内は URL から
	 * 来た対象が見つからないときだけ（一度は見つかった対象が消えたときは出さない）。
	 */
	let lastFoundTarget: RecordTarget = { type: 'all' };
	$effect(() => {
		const search = page.url.search;
		const loaded = registryLoaded;
		const currentConnections = connectionRows;
		const currentGroups = groupRows;
		const settingsAvailable = canWrite;
		if (viewNavsInFlight > 0) return;
		const parsed = parseRecordsView(new URLSearchParams(search));
		const resolved = resolveRecordsView(parsed, currentConnections, currentGroups, {
			loaded,
			settingsAvailable
		});
		untrack(() => {
			if (loaded && !resolved.missingTarget) lastFoundTarget = resolved.view.target;
			const wasFound = parsed.target !== null && sameRecordTarget(parsed.target, lastFoundTarget);
			if (loaded && resolved.missingTarget && !wasFound) {
				toastStore.push('info', MISSING_RECORD_TARGET_MESSAGE);
			}
			if (!sameRecordsView(currentView(), resolved.view)) {
				discardForView();
				applyView(resolved.view);
			}
			if (loaded && search !== recordsViewSearch(resolved.view)) {
				void navigateToView(resolved.view, { replace: true });
			}
		});
	});

	afterNavigate((nav) => {
		// 戻る・進むで来た項目は、どの操作で積んだものか分からない。
		if (nav.type === 'popstate') viewEntryFromKeyNavigation = false;
	});

	/**
	 * ページの `beforeNavigate`。戻る・進むなどでページ内の対象・タブが変わるときの
	 * 未保存の確認（`confirmViewChange`）をする。
	 */
	function handleBeforeNavigate(nav: BeforeNavigate): void {
		const to = nav.to?.url;
		if (!to) return;
		// ページ内の `goto`: 呼ぶ前に `requestView` などが確かめ済み。
		if (viewNavHrefs.has(to.href)) return;
		if (!nav.willUnload && to.pathname === page.url.pathname) {
			const next = resolveRecordsView(
				parseRecordsView(to.searchParams),
				connectionRows,
				groupRows,
				{
					loaded: registryLoaded,
					settingsAvailable: canWrite
				}
			).view;
			if (!confirmViewChange(next)) nav.cancel();
		}
		// 別のページ・再読み込みは guard に任せる。
	}
	beforeNavigate(handleBeforeNavigate);

	// 未保存の入力の確認（banto #214 の流儀、ChronoGazer #508）: 右ペインのフォーム
	// （記録項目の編集＋しきい値、または作成フォーム）と「設定」タブのフォームの
	// どちらかに、読み込み/保存時の値と違う入力があれば未保存。出ていないフォームは
	// 数えない（閉じた・切り替えたときに捨てている）。保存の成功は `markClean()`
	// （編集）か新しいストアへの差し替え（新規作成）で消す。保存・作成の最中も
	// 「保存が落ちるかもしれない」ので未保存として扱う。
	guardUnsavedChanges({
		isDirty: () => rightPaneDirty() || settingsDirty(),
		isSaving: () => rightPaneBusy() || settingsBusy(),
		isSamePage: isSamePath
	});

	/** 「設定」タブに出す接続先・収集グループ（一覧から引き直す）。 */
	const settingsConnection = $derived.by((): PlcConnection | null => {
		if (target.type !== 'connection') return null;
		const id = target.id;
		return connectionRows.find((c) => c.id === id) ?? null;
	});
	const settingsGroup = $derived.by((): CollectionGroup | null => {
		if (target.type !== 'group') return null;
		const id = target.id;
		return groupRows.find((g) => g.id === id) ?? null;
	});

	// 対象が変わった（または `discardForView` で外した）ら、「設定」タブのフォームを
	// 今の対象の保存済みの値で作り直す。一覧の再読み込みで同じ id の行が差し替わった
	// だけなら作り直さない（入力中の値を消さない）。
	$effect(() => {
		const connection = settingsConnection;
		const loadedId = selectedConnection?.id;
		if (canWrite && connection !== null && loadedId !== connection.id) {
			untrack(() => loadConnectionSettings(connection));
		}
	});
	$effect(() => {
		const group = settingsGroup;
		const loadedId = selectedGroup?.id;
		if (canWrite && group !== null && loadedId !== group.id) {
			untrack(() => loadGroupSettings(group));
		}
	});

	/** 中央のタブ。「すべて」と閲覧者には「設定」が無い（タブ列ごと出さない - `hideSingleTab`）。 */
	const viewTabs = $derived(
		target.type === 'all' || !canWrite
			? [{ id: 'list', label: '記録項目' }]
			: [
					{ id: 'list', label: '記録項目' },
					{ id: 'settings', label: '設定' }
				]
	);

	/**
	 * `Tabs` の `value` の関数バインディングの setter。未保存の確認でキャンセル
	 * されたら `activeTab` は変わらず、getter が元のタブを返す（HUB と同じ）。
	 */
	function setActiveTabFromTabs(id: string | undefined): void {
		requestView({ target, tab: id === 'settings' ? 'settings' : 'list' });
	}

	const treeSelectedId = $derived(recordTargetNodeId(target));

	function handleTreeSelect(data: RecordTreeNodeData, info: { navigation: boolean }): void {
		const nextTarget = recordTargetOf(data);
		const next: RecordsView = {
			target: nextTarget,
			tab: nextTarget.type === 'all' ? 'list' : activeTab
		};
		const moved = requestView(next, { keyNavigation: info.navigation });
		// 狭幅では、選んだらドロワーを閉じて中央を見せる（キーの移動では閉じない）。
		if (moved && layoutNarrow && !info.navigation) treeOpen = false;
	}

	/** ツリーを閉じていても何で絞っているか分かるよう、枠の帯に出す名前。 */
	const treeSelectionLabel = $derived.by((): string => {
		if (target.type === 'connection') return settingsConnection?.name ?? 'すべて';
		if (target.type === 'group') return settingsGroup?.name ?? 'すべて';
		return 'すべて';
	});

	/** 左ペインとそのトグルの名前。 */
	const TREE_PANE_LABEL = '接続先と収集グループ';

	// 層の約束（`@banto/ui` の `layering.ts`）: 下位の層（枠の中のドロワー）を開くなら
	// 上位の層（サイドバー）を先に畳む（HUB の tags ページと同じ。開いた遷移で畳む）。
	$effect(() => {
		if (!treeOpen) return;
		untrack(() => {
			if (mobileNavStore.open) mobileNavStore.closeNav();
		});
	});

	/** ツリーの選択で絞った記録項目。 */
	const visibleTagRows = $derived(filterRecordsByTarget(tagRows, target, groupRows));

	// --- #414 段階2: 収集の開始時に外される設定 --------------------------------
	//
	// 判定は Rust（収集の開始と同じ `build_config_lenient_from`）の結果を読む
	// だけ（`listConfigExclusions`）。収集が走っていなくても出す。再取得の
	// 作法: 前回の結果を持ち越さず、読めて
	// いないのに「不正な設定はありません」と言わない。

	let exclusionMarks: ExclusionView[] | null = $state(null);
	let exclusionMarksError: string | null = $state(null);
	let exclusionMarksLoading = $state(false);
	let exclusionMarksGeneration = 0;
	const exclusionMarksState: ListLoadState<ExclusionView> = $derived({
		items: exclusionMarks,
		error: exclusionMarksError
	});
	const exclusionMarksView = $derived(configExclusionsView(exclusionMarksState));

	async function reloadConfigExclusions(): Promise<void> {
		exclusionMarksGeneration += 1;
		const generation = exclusionMarksGeneration;
		exclusionMarksLoading = true;
		const started = coverageReloadStarted<ExclusionView>();
		exclusionMarks = started.items;
		exclusionMarksError = started.error;
		const outcome = await runGuardedListLoad(
			generation,
			listConfigExclusions(),
			() => exclusionMarksGeneration
		);
		let settled: ListLoadState<ExclusionView>;
		switch (outcome.kind) {
			case 'applied':
				settled = coverageReloadSettled(started, { kind: 'applied', items: outcome.items });
				break;
			case 'error':
				settled = coverageReloadSettled(started, {
					kind: 'error',
					message: errorMessage(outcome.err)
				});
				break;
			case 'stale':
				return;
		}
		exclusionMarks = settled.items;
		exclusionMarksError = settled.error;
		exclusionMarksLoading = false;
	}

	$effect(() => {
		void reloadDrivers();
		void reloadConnections();
		void reloadGroups();
		void reloadTags();
	});
</script>

<div class="page">
	<h2>記録</h2>

	<!--
			#414 段階2: 今の設定で収集を開始したら外されるもの。判定は Rust の
			結果をそのまま出す（文言はタグなら保存時の拒否理由と同じ）。0 件なら
			出さない。読めていない・読めなかったときは「無い」と言わない。
		-->
	{#if exclusionMarksView !== 'none'}
		<section class="config-exclusions" aria-label="収集の開始時に外される設定">
			<h3>収集の開始時に外される設定</h3>
			{#if exclusionMarksView === 'loading'}
				<p class="loading">判定を読み込み中…</p>
			{:else if exclusionMarksView === 'failed'}
				<p class="load-error" role="alert">
					判定を読み込めませんでした（{exclusionMarksError}）。不正な設定が無いという意味ではありません。
					<button
						type="button"
						onclick={() => void reloadConfigExclusions()}
						disabled={exclusionMarksLoading}
					>
						再試行
					</button>
				</p>
			{:else}
				<p class="note exclusion-note">
					次の設定は不正なため、収集の開始時に外されます（残りは収集されます）。直すと、次の「収集を再起動」から収集されます。
				</p>
				<ul class="exclusion-marks">
					{#each listRows(exclusionMarksState) as mark (mark.key)}
						<li>
							<span class="exclusion-unit">{exclusionUnitLabel(mark.unit)}</span>
							<strong>{mark.name}</strong>: {mark.message}
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	{/if}

	<!--
			#1179 PR4b: 右ペイン。記録項目の詳細（編集・しきい値・削除）か、作成
			フォーム（記録項目・接続先・収集グループ）のどれか 1 つ。編集者以上の
			ときだけ開く（閲覧者は行を選んでも開かない）。閉じるのはヘッダーの
			「閉じる」と、作成・削除の成功の後（未保存なら確かめる）。
		-->
	{#snippet rightPaneContent()}
		<div class="detail-pane" data-testid="record-detail-pane">
			<div class="detail-pane-header">
				<h3>{rightPaneTitle}</h3>
				<button
					type="button"
					class="pane-close"
					onclick={requestCloseRightPane}
					disabled={rightPaneBusy()}
					aria-label="閉じる"
				>
					×
				</button>
			</div>
			<div class="detail-pane-body">
				{#if rightPane === 'tag' && selectedTag && canWrite}
					<UnsavedChangesNotice pending={editTagStore.isDirty} label={UNSAVED_NOTICE} />
					{#if selectedTagIsHub}
						<p class="note">
							Hub 経由のタグです。アドレスは Hub
							のタグ名（外部名）で、スケーリングは設定できません（Hub は工学値を送ります）。
						</p>
					{/if}
					<BantoForm
						schema={tagSchema(TAG_EDIT, selectedTagIsHub)}
						store={editTagStore}
						onSubmit={saveTag}
						submitting={savingTag || deletingTag}
						submitLabel="保存"
					>
						<button
							type="button"
							class="danger"
							onclick={handleDeleteTag}
							disabled={savingTag || deletingTag || savingThresholds}
						>
							削除
						</button>
					</BantoForm>

					<div class="thresholds-pane">
						<h4>しきい値（記録計の設定）</h4>
						<p class="note">
							タグの定義とは別に保存します。空欄は設定なしです（色・帯・しきい値のイベントを出しません）。収集中なら「収集を再起動」で反映されます。
						</p>
						{#if thresholdsError !== null}
							<p class="load-error" role="alert">
								しきい値を読めませんでした（{thresholdsError}）。
								<button type="button" onclick={() => selectedTag && loadThresholds(selectedTag.id)}>
									再読み込み
								</button>
							</p>
						{:else if selectedThresholds === null}
							<p class="loading">しきい値を読み込んでいます…</p>
						{:else}
							<UnsavedChangesNotice pending={editThresholdsStore.isDirty} label={UNSAVED_NOTICE} />
							<BantoForm
								schema={thresholdSchema(TAG_THR)}
								store={editThresholdsStore}
								onSubmit={saveThresholds}
								submitting={savingThresholds || savingTag || deletingTag}
								submitLabel="しきい値を保存"
							/>
						{/if}
					</div>
				{:else if rightPane === 'createTag' && canWrite}
					<UnsavedChangesNotice pending={createTagStore.isDirty} label={UNSAVED_NOTICE} />
					{#if createTagIsHub}
						<p class="note">
							Hub 経由のグループです。アドレスは Hub
							のタグ名（外部名）で、スケーリングは設定できません（Hub は工学値を送ります）。
						</p>
					{/if}
					<BantoForm
						schema={tagSchema(TAG_CREATE, createTagIsHub)}
						store={createTagStore}
						onSubmit={handleCreateTag}
						submitting={creatingTag}
						submitLabel="作成"
					/>
				{:else if rightPane === 'createConnection' && canWrite}
					{#if driversError}
						<p class="load-error" role="alert">
							ドライバ一覧を読み込めませんでした（{driversError}）。<button
								type="button"
								onclick={() => void reloadDrivers()}
							>
								再試行
							</button>
						</p>
					{:else if drivers === null}
						<p class="loading">ドライバ一覧を読み込み中…</p>
					{:else if protocolOptions.length === 0}
						<p class="load-error" role="alert">利用できるドライバがありません。</p>
					{:else}
						<p class="note">
							PLC に直接つなぐ接続先です。Hub 経由の接続先は「記録項目」の「Hub
							から追加」で作られます。
						</p>
						<UnsavedChangesNotice
							pending={createConnectionStore.isDirty ||
								Object.values(createDriverState.touched).some(Boolean)}
							label={UNSAVED_NOTICE}
						/>
						<BantoForm
							schema={connectionSchema(PLC_CREATE)}
							store={createConnectionStore}
							onSubmit={handleCreateConnection}
							submitting={creatingConnection}
							submitLabel="作成"
						>
							{#if driverFor(String(createConnectionStore.values[`${PLC_CREATE}Protocol`] ?? ''))}
								{@const driver = driverFor(
									String(createConnectionStore.values[`${PLC_CREATE}Protocol`] ?? '')
								)!}
								<DriverSettingsForm
									schema={driver.settingsSchema.fields}
									bind:value={createDriverState}
									errors={createDriverErrors}
									readOnly={creatingConnection}
									idPrefix="plc-create-driver"
								/>
							{/if}
						</BantoForm>
					{/if}
				{:else if rightPane === 'createGroup' && canWrite}
					<!-- #394 レビュー P1-3: 「先に作成してください」は依存先を
						**読めて0件**のときだけ。読めていないときにこれを出すと、
						既にある接続を重複登録させてしまう。 -->
					{#if groupCreateGate === 'needs-prerequisite'}
						<p class="note">
							先に接続先を1件以上作成してください（Hub 経由の接続先は「記録項目」の「Hub
							から追加」で作られます）。
						</p>
					{:else if groupCreateGate === 'dependency-loading'}
						<p class="note">接続先の一覧を読み込んでいます…</p>
					{:else if groupCreateGate === 'dependency-failed'}
						<p class="load-error" role="alert">
							接続先の一覧を読み込めなかったため、作成フォームを表示できません（{connectionsError}）。
							<button
								type="button"
								onclick={() => void reloadConnections()}
								disabled={connectionsLoading}
							>
								再試行
							</button>
						</p>
					{:else}
						<UnsavedChangesNotice pending={createGroupStore.isDirty} label={UNSAVED_NOTICE} />
						<BantoForm
							schema={groupSchema(GROUP_CREATE)}
							store={createGroupStore}
							onSubmit={handleCreateGroup}
							submitting={creatingGroup}
							submitLabel="作成"
						/>
					{/if}
				{/if}
			</div>
		</div>
	{/snippet}

	<!--
			#1179 PR4b: 左ツリー・中央タブ・右の詳細の 3 ペイン（`@banto/ui` の
			`ThreePaneLayout`）。狭幅の判定はビューポート（`mobileNavStore.isNarrow`、
			HUB と同じ）。退避・トグル・Esc・スクリム・フォーカスの出入りは部品が持つ。
			帯には、ツリーを閉じていても何で絞っているか分かるよう、選択中の名前を出す。
		-->
	<div class="records-frame">
		<ThreePaneLayout
			narrowOverride={mobileNavStore.isNarrow}
			bind:leftDrawerOpen={treeOpen}
			bind:rightDrawerOpen
			bind:narrow={layoutNarrow}
			right={rightPane !== null && canWrite ? rightPaneContent : undefined}
			messages={{
				threePaneLeft: () => TREE_PANE_LABEL,
				threePaneResizeLeft: () => `${TREE_PANE_LABEL}の幅`,
				threePaneRight: () => rightPaneTitle,
				threePaneResizeRight: () => `${rightPaneTitle}の幅`
			}}
		>
			{#snippet left()}
				<div class="tree-pane" data-testid="record-tree-pane">
					{#if canWrite}
						<div class="tree-toolbar">
							<button type="button" class="secondary" onclick={openCreateConnection}>
								接続先を追加
							</button>
							<button type="button" class="secondary" onclick={openCreateGroup}>
								収集グループを追加
							</button>
						</div>
					{/if}
					<!-- #394 レビュー P1-3: 「読めていない」を「0件」として描かない。 -->
					{#if connectionsView === 'failed'}
						<p class="load-error" role="alert">
							接続先の一覧を読み込めませんでした（{connectionsError}）。登録が0件という意味ではありません。
							<button
								type="button"
								onclick={() => void reloadConnections()}
								disabled={connectionsLoading}
							>
								再試行
							</button>
						</p>
					{/if}
					{#if groupsView === 'failed'}
						<p class="load-error" role="alert">
							収集グループの一覧を読み込めませんでした（{groupsError}）。登録が0件という意味ではありません。
							<button type="button" onclick={() => void reloadGroups()} disabled={groupsLoading}>
								再試行
							</button>
						</p>
					{/if}
					{#if connectionsRetry}
						<p class="load-error" role="alert">
							接続先の一覧を更新できませんでした（{connectionsError}）。表示は最後に読み込めた内容です。
							<button
								type="button"
								onclick={() => void reloadConnections()}
								disabled={connectionsLoading}
							>
								再試行
							</button>
						</p>
					{/if}
					{#if groupsRetry}
						<p class="load-error" role="alert">
							収集グループの一覧を更新できませんでした（{groupsError}）。表示は最後に読み込めた内容です。
							<button type="button" onclick={() => void reloadGroups()} disabled={groupsLoading}>
								再試行
							</button>
						</p>
					{/if}
					{#if connectionsView === 'loading' || groupsView === 'loading'}
						<p class="loading">読み込み中…</p>
					{:else if connectionsView !== 'failed' && groupsView !== 'failed'}
						<RecordTree
							connections={connectionRows}
							groups={groupRows}
							records={tagRows}
							selectedId={treeSelectedId}
							selectionFollowsFocus={!layoutNarrow}
							onselect={handleTreeSelect}
						/>
					{/if}
				</div>
			{/snippet}
			{#snippet titleContent()}
				<span class="tree-selection" data-testid="record-tree-selection">
					{treeSelectionLabel}
				</span>
			{/snippet}
			<div class="center-pane">
				<!--
						中央はタブで「記録項目」と「設定」を切り替える。「すべて」と閲覧者には
						「設定」が無いので、タブ列ごと隠す（`hideSingleTab`）。`value` は関数
						バインディング: 未保存の確認でキャンセルされたら、getter が元のタブを
						返す（`setActiveTabFromTabs`、HUB と同じ）。
					-->
				<div class="view-tabs" class:single={viewTabs.length === 1}>
					<Tabs
						tabs={viewTabs}
						hideSingleTab
						bind:value={() => activeTab, setActiveTabFromTabs}
						label="表示の切り替え"
					>
						{#snippet panel(id)}
							{#if id === 'list'}
								<div class="list-pane">
									<div class="toolbar">
										{#if canWrite}
											{#if registrationGroup}
												<button type="button" onclick={openCreateTag}>記録項目を追加</button>
											{/if}
											<!-- #383 段階3 P4: Hub のカタログから登録する。 -->
											<button
												type="button"
												class="secondary"
												onclick={() => (hubDialogOpen = true)}
											>
												Hub から追加
											</button>
										{/if}
									</div>
									{#if canWrite && !registrationGroup}
										<p class="note" data-testid="record-add-hint">
											左のツリーで収集グループを選ぶと、記録項目を追加できます。
										</p>
									{/if}
									<p class="note">
										{canWrite
											? '行をクリックすると右に詳細が表示されます。'
											: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
									</p>
									{#if hasSimulationConnection}
										<!-- #413: 記録されないことを常に分かるようにする。 -->
										<p class="note simulation-note" role="note">
											ツリーで「シミュレーション」の印が付いた接続先は、実機の代わりに内蔵シミュレータへ接続します。値は現在値・イベントには出ますが、データファイルには記録されません。切替は「収集を再起動」で反映されます（実際の動作は「設定
											&gt; 収集」の接続ごとの状態で確認できます）。
										</p>
									{/if}
									{#if tagsView === 'loading'}
										<p class="loading">読み込み中…</p>
									{:else if tagsView === 'failed'}
										<p class="load-error" role="alert">
											記録項目の一覧を読み込めませんでした（{tagsError}）。登録が0件という意味ではありません。
											<button
												type="button"
												onclick={() => void reloadTags()}
												disabled={tagsLoading}
											>
												再試行
											</button>
										</p>
									{:else}
										{#if tagsRetry}
											<p class="load-error" role="alert">
												一覧を更新できませんでした（{tagsError}）。表示は最後に読み込めた内容です。
												<button
													type="button"
													onclick={() => void reloadTags()}
													disabled={tagsLoading}
												>
													再試行
												</button>
											</p>
										{/if}
										<!-- #391 レビュー A / #394 追補: 保存中・削除中は行の選択を
											操作できないようにする（`selectTag` 自体のガードに加え、
											見た目でも伝える）。 -->
										<div
											class="grid-wrap"
											data-testid="record-list"
											class:saving={savingTag || deletingTag}
											aria-disabled={savingTag || deletingTag}
										>
											<BantoGrid
												rows={visibleTagRows}
												columns={tagColumns}
												getRowId={(t) => t.id}
												onRowClick={canWrite ? selectTag : undefined}
											/>
										</div>
									{/if}
								</div>
							{:else}
								<div class="settings-pane" data-testid="record-settings">
									{#if target.type === 'connection' && selectedConnection && canWrite}
										{#if selectedConnectionIsHub}
											<!-- #383 段階3 P4: Hub 接続は読み取り専用（ホスト・ポートを持たない）。 -->
											<h3>{selectedConnection.name}（Hub 経由の接続先）</h3>
											<p class="note">
												Hub から受信するタグの入れ物です。接続先と API キーは「設定 &gt;
												Hub接続」（管理者）で設定します。記録項目は「記録項目」の「Hub
												から追加」で登録します。配下に収集グループがある間は削除できません。
											</p>
											<UnsavedChangesNotice
												pending={editHubConnectionStore.isDirty}
												label={UNSAVED_NOTICE}
											/>
											<BantoForm
												schema={hubConnectionSchema(HUB_EDIT)}
												store={editHubConnectionStore}
												onSubmit={saveHubConnection}
												submitting={savingConnection || deletingConnection}
												submitLabel="保存"
											>
												<button
													type="button"
													class="danger"
													onclick={handleDeleteConnection}
													disabled={savingConnection || deletingConnection}
												>
													削除
												</button>
											</BantoForm>
										{:else}
											<h3>{selectedConnection.name} を編集</h3>
											<!-- #413: 一覧の「動作」列の代わり（レジストリの値）。 -->
											<p class="note" data-testid="record-connection-mode">
												動作: {connectionModeLabel(selectedConnection.simulation)}
											</p>
											{#if driversError}
												<!-- ドライバの欄はドライバ一覧が読めてから出る（保存済みの値は残る）。 -->
												<p class="load-error" role="alert">
													ドライバ一覧を読み込めませんでした（{driversError}）。<button
														type="button"
														onclick={() => void reloadDrivers()}
													>
														再試行
													</button>
												</p>
											{/if}
											<UnsavedChangesNotice
												pending={editConnectionStore.isDirty ||
													Object.values(editDriverState.touched).some(Boolean)}
												label={UNSAVED_NOTICE}
											/>
											<BantoForm
												schema={connectionSchema(PLC_EDIT)}
												store={editConnectionStore}
												onSubmit={saveConnection}
												submitting={savingConnection || deletingConnection}
												submitLabel="保存"
											>
												{#if driverFor(String(editConnectionStore.values[`${PLC_EDIT}Protocol`] ?? ''))}
													{@const driver = driverFor(
														String(editConnectionStore.values[`${PLC_EDIT}Protocol`] ?? '')
													)!}
													<DriverSettingsForm
														schema={driver.settingsSchema.fields}
														bind:value={editDriverState}
														errors={editDriverErrors}
														readOnly={savingConnection || deletingConnection}
														idPrefix="plc-edit-driver"
													/>
												{/if}
												<button
													type="button"
													class="danger"
													onclick={handleDeleteConnection}
													disabled={savingConnection || deletingConnection}
												>
													削除
												</button>
											</BantoForm>
										{/if}
									{:else if target.type === 'group' && selectedGroup && canWrite}
										<h3>{selectedGroup.name} を編集</h3>
										<UnsavedChangesNotice pending={editGroupStore.isDirty} label={UNSAVED_NOTICE} />
										<BantoForm
											schema={groupSchema(GROUP_EDIT)}
											store={editGroupStore}
											onSubmit={saveGroup}
											submitting={savingGroup || deletingGroup}
											submitLabel="保存"
										>
											<button
												type="button"
												class="danger"
												onclick={handleDeleteGroup}
												disabled={savingGroup || deletingGroup}
											>
												削除
											</button>
										</BantoForm>
									{:else}
										<p class="loading">読み込み中…</p>
									{/if}
								</div>
							{/if}
						{/snippet}
					</Tabs>
				</div>
			</div>
		</ThreePaneLayout>
	</div>

	{#if canWrite}
		<HubImportDialog
			open={hubDialogOpen}
			connections={connectionRows}
			groups={groupRows}
			onclose={() => (hubDialogOpen = false)}
			onregistered={() => void afterHubRegistration()}
		/>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		height: calc(100vh - var(--banto-shell-header-height) - 2.5rem);
		min-height: 0;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	h3 {
		margin: 0;
		font-size: 0.95rem;
	}

	h4 {
		margin: 0 0 0.5rem;
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	/* 3 ペインの枠が残りの高さを埋める（グリッド・ツリーの高さの基準）。 */
	.records-frame {
		flex: 1;
		min-height: 360px;
		display: flex;
		flex-direction: column;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		overflow: hidden;
	}

	.tree-pane {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		height: 100%;
		min-height: 0;
		box-sizing: border-box;
		padding: 0.5rem;
	}

	.tree-pane > :global(.tree-container) {
		flex: 1;
		min-height: 0;
	}

	.tree-toolbar {
		flex: 0 0 auto;
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.tree-toolbar button {
		font-size: 0.78rem;
		padding: 0 0.6rem;
	}

	.tree-selection {
		font-weight: 600;
	}

	.center-pane {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		box-sizing: border-box;
		padding: 0.75rem 1rem;
	}

	/* 表示中のタブのパネルが残りの高さを埋める（HUB の tags ページと同じ）。 */
	.view-tabs {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
	}

	.view-tabs > :global(.banto-tabs__panel:not([hidden])) {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
	}

	.view-tabs.single > :global(.banto-tabs__panel) {
		padding-block-start: 0;
	}

	.list-pane {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
		gap: 0.5rem;
	}

	.toolbar {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 0.6rem;
		flex-wrap: wrap;
	}

	.toolbar button:not(.secondary) {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 1rem;
		background: var(--banto-primary);
		border: 1px solid var(--banto-primary);
		border-radius: var(--banto-radius-md);
		color: var(--banto-on-primary, #fff);
		cursor: pointer;
	}

	.settings-pane {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		overflow: auto;
		max-width: 48rem;
	}

	.detail-pane {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
	}

	.detail-pane-header {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.75rem 1rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.detail-pane-header h3 {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pane-close {
		border: none;
		background: none;
		color: var(--banto-text-muted);
		font-size: 1.25rem;
		line-height: 1;
		padding: 0.15rem 0.4rem;
		cursor: pointer;
		border-radius: var(--banto-radius);
	}

	.pane-close:hover {
		color: var(--banto-text);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.pane-close:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.detail-pane-body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 0.75rem 1rem 1.25rem;
	}

	button.secondary {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 1rem;
		background: transparent;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		cursor: pointer;
	}

	button.secondary:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.thresholds-pane {
		margin-top: 1rem;
		padding-top: 0.75rem;
		border-top: 1px solid var(--banto-border);
	}

	.note {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	/* #394 レビュー P1-3: 読み込みに失敗した一覧は「0件」ではなく、
	この行（失敗の事実 + 再試行の導線）として出す。 */
	.load-error {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	.load-error button {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.75rem;
		background: transparent;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		cursor: pointer;
	}

	.load-error button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.grid-wrap {
		flex: 1;
		min-height: 200px;
	}

	/* #413: 「値は記録されません」は見落とすと困るので、薄いグレーの
	`.note` より目に入る色にする（エラーではないので danger は使わない）。 */
	.simulation-note {
		color: var(--banto-text);
	}

	/* #414 段階2: 不正な設定は収集から外れる（黙って欠測する）ので、エラー
	ほど強くはないが見落とさない色・枠で出す。3 ペインの高さを奪いすぎない
	よう、長いときはこの枠の中でスクロールする。 */
	.config-exclusions {
		flex: 0 0 auto;
		max-height: 30vh;
		overflow: auto;
		background: var(--banto-surface);
		border: 1px solid var(--banto-danger);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	.exclusion-note {
		color: var(--banto-text);
		margin-bottom: 0.5rem;
	}

	.exclusion-marks {
		margin: 0;
		padding-left: 1.25rem;
		font-size: 0.8rem;
	}

	.exclusion-unit {
		display: inline-block;
		min-width: 6.5em;
		color: var(--banto-text-muted);
	}

	button.danger {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 1.25rem;
		background: transparent;
		border: 1px solid var(--banto-danger);
		border-radius: var(--banto-radius-md);
		color: var(--banto-danger);
		font-weight: 600;
		cursor: pointer;
	}

	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}

	/* #391 レビュー A: 保存中は行を操作できないようにする（既存の保存
	ボタンの disabled 表現 - opacity + cursor - に合わせる）。 */
	button.danger:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.grid-wrap.saving {
		opacity: 0.5;
		pointer-events: none;
		cursor: not-allowed;
	}
</style>
