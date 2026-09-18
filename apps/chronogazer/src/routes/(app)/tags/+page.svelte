<script lang="ts">
	/**
	 * タグ設定画面（#383 段階2a / R1-B、recorder-requirements.md §6）。
	 *
	 * 手本は `routes/(app)/users/+page.svelte`: `BantoGrid`（一覧）+
	 * `BantoForm`/`createFormStore`（新規作成・編集）+ 行クリックで下に編集
	 * パネル。独自の Drawer / Modal / SplitPane / ツリーは作らない -
	 * このアプリの `lib/components/` には4部品しか無く、このPRでも増やさない。
	 *
	 * 構成は PLC接続 / 収集グループ / タグ の3セクションを**縦並び**（タブでは
	 * ない）で、FK の依存順（接続 → グループ → タグ）に並べる。理由: このアプリ
	 * に既存のタブ部品が無く新規に作ると部品を増やしてしまうこと、縦並びなら
	 * 「まず接続を作る→その接続がグループのプルダウンに出る→まずグループを作る
	 * →そのグループがタグのプルダウンに出る」という作成の自然な流れをそのまま
	 * 画面の上から下への流れに一致させられること。
	 *
	 * viewer は閲覧のみ（`canWriteResources` = editor 以上の判定、
	 * `$lib/permissions` 既存）: 新規作成フォーム・編集パネル・削除ボタンを
	 * 出さない。削除は `window.confirm`（users 画面と同じ出し方）。
	 *
	 * 収集グループの周期は `banto_tags::ALLOWED_PERIOD_MS` と一致する固定選択
	 * 肢（`tagRegistryAdmin.ts`の`ALLOWED_PERIOD_MS`）。
	 *
	 * 一覧は全件取得のクライアントモード（`BantoGrid`のページングは使わない）
	 * - 256タグ程度なら現実的（R0のv1目標、指示書どおりこのPRではこれで良い。
	 * サーバーモードのページングはR1-C以降の実データ時に検討）。
	 *
	 * **フィールド名の接頭辞について（E2Eで実際に踏んだ事故）**: PLC接続 /
	 * 収集グループ / タグ の3セクションが常に同時に描画され、かつ各セクション
	 * は「新規作成フォーム」と「編集パネル」も選択中は同時に描画される -
	 * つまり最大 6 個の `BantoForm` インスタンスが同一ページに同時に存在しうる。
	 * `@banto/forms`の各フィールドは `id={def.name}`（例: `"name"`）を持つため、
	 * スキーマのフィールド名をそのまま使うと6インスタンス全部で `id="name"`
	 * が重複し、`<label for="name">` の紐付けがブラウザの
	 * `getElementById`相当の解決で最初の1個にしか行かず、残りの5個は
	 * ラベル無しになる（実際に E2E で「収集グループの名前欄が見つからない」
	 * という形で踏んだ - 一覧・保存自体は動くが、フォームのアクセシビリティ
	 * ラベルが壊れる）。`users`/`plc-connections`（relay-wright）の先例は
	 * 編集パネルを生 HTML（`<label>`の暗黙紐付け）にすることでこの衝突を
	 * 避けているが、この画面は3エンティティ×2（作成/編集）が同時に存在する
	 * ため、それでも接続×グループ×タグの3つの「名前」欄同士は衝突する。
	 * そこで各エンティティ×モードごとに一意な接頭辞（`plcCreate`/`plcEdit`/
	 * `groupCreate`/`groupEdit`/`tagCreate`/`tagEdit`）をスキーマのフィールド
	 * 名自体に付け、`to*Input`/`*FormValues` 側で wire のフィールド名と
	 * 相互変換する。
	 */
	import { untrack } from 'svelte';
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { BantoForm, createFormStore, type FormSchema } from '@banto/forms';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listPlcConnections,
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
		isTagRegistryAvailable,
		DEMO_MODE_MESSAGE,
		ALLOWED_PERIOD_MS,
		type PlcConnection,
		type PlcConnectionInput,
		type PlcProtocol,
		type WordOrder,
		type CollectionGroup,
		type CollectionGroupInput,
		type Tag,
		type TagInput,
		type TagDataType
	} from '$lib/banto/tagRegistryAdmin';
	import {
		runGuardedSave,
		runGuardedDelete,
		schemaWireFields,
		splitServerFieldErrors,
		joinFieldErrorMessages,
		type SaveGuardToken,
		type DeleteGuardToken
	} from './tagsPageLogic';

	const available = isTagRegistryAvailable();
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

	const protocolOptions: { value: PlcProtocol; label: string }[] = [
		{ value: 'modbus-tcp', label: 'Modbus TCP' },
		{ value: 'slmp', label: 'SLMP（MELSEC MCプロトコル）' }
	];

	// `@banto/forms`のSelectFieldは値なし用の固定プレースホルダ
	// `<option value="">選択してください</option>`を常に持つ - wire上の
	// 「未指定」を表す `''`（`PlcConnectionInput::word_order`のdoc comment
	// 参照）をそのままオプション値に使うと、このプレースホルダと value が
	// 衝突し、ブラウザは先勝ちでプレースホルダ側を選択してしまう（実機で
	// 確認済み: 既定を選んでも `<select>` が「選択してください」のまま表示
	// される）。UI専用のセンチネル `WORD_ORDER_AUTO`（wire には出さない）で
	// 衝突を避け、`toPlcConnectionInput`/`connectionFormValues` で wire の
	// `''` と相互変換する。
	const WORD_ORDER_AUTO = 'auto';
	const wordOrderOptions: { value: string; label: string }[] = [
		{ value: WORD_ORDER_AUTO, label: '既定（プロトコルに合わせる）' },
		{ value: 'low_high', label: 'Low-High' },
		{ value: 'high_low', label: 'High-Low' }
	];

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
					default: 'modbus-tcp',
					options: protocolOptions
				},
				{
					name: `${prefix}Host`,
					label: 'ホスト',
					type: 'text',
					required: true,
					placeholder: '192.168.1.10'
				},
				{
					name: `${prefix}Port`,
					label: 'ポート',
					type: 'number',
					required: true,
					min: 1,
					max: 65535,
					default: 502
				},
				{
					name: `${prefix}UnitId`,
					label: 'ユニットID',
					type: 'number',
					min: 0,
					max: 255,
					default: 1
				},
				{
					name: `${prefix}WordOrder`,
					label: 'ワードオーダー',
					type: 'select',
					default: WORD_ORDER_AUTO,
					options: wordOrderOptions
				},
				{ name: `${prefix}Enabled`, label: '有効', type: 'checkbox', default: true }
			]
		};
	}

	function toPlcConnectionInput(
		prefix: string,
		values: Record<string, unknown>
	): PlcConnectionInput {
		const wordOrder = values[`${prefix}WordOrder`];
		const port = values[`${prefix}Port`];
		const unitId = values[`${prefix}UnitId`];
		return {
			name: String(values[`${prefix}Name`] ?? ''),
			protocol: (values[`${prefix}Protocol`] as PlcProtocol) ?? 'modbus-tcp',
			host: String(values[`${prefix}Host`] ?? ''),
			port: typeof port === 'number' ? port : 0,
			unitId: typeof unitId === 'number' ? unitId : 1,
			enabled: Boolean(values[`${prefix}Enabled`]),
			wordOrder: wordOrder === WORD_ORDER_AUTO || wordOrder == null ? '' : (wordOrder as WordOrder)
		};
	}

	/** 編集フォームの初期値（`toPlcConnectionInput`の逆）。wire の `wordOrder`（`''` = 未指定）を選択肢の `WORD_ORDER_AUTO` へ変換する。 */
	function connectionFormValues(prefix: string, conn: PlcConnection): Record<string, unknown> {
		return {
			[`${prefix}Name`]: conn.name,
			[`${prefix}Protocol`]: conn.protocol,
			[`${prefix}Host`]: conn.host,
			[`${prefix}Port`]: conn.port,
			[`${prefix}UnitId`]: conn.unitId,
			[`${prefix}WordOrder`]: conn.wordOrder === '' ? WORD_ORDER_AUTO : conn.wordOrder,
			[`${prefix}Enabled`]: conn.enabled
		};
	}

	// スキーマに実在する wire フィールド名一覧（`applyServerErrors`が
	// `scaling`のようなスキーマ外のフィールドをトーストへ振り分けられる
	// ようにする）。CREATE/EDIT どちらのプレフィックスで計算しても剥がした
	// 後の wire 名は同じなので、ここでは PLC_CREATE 側で一度だけ計算する。
	const CONNECTION_WIRE_FIELDS = schemaWireFields(PLC_CREATE, connectionSchema(PLC_CREATE).fields);

	let connections: PlcConnection[] = $state([]);
	let connectionsLoading = $state(false);

	async function reloadConnections(): Promise<void> {
		if (!available) return;
		connectionsLoading = true;
		try {
			connections = await listPlcConnections();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			connectionsLoading = false;
		}
	}

	// `untrack`: 初期スナップショットで十分（テンプレート側の `schema` prop は
	// 直接 `connectionSchema(...)` を呼ぶので常に最新 - Svelteの
	// `state_referenced_locally`警告を意図どおり黙らせる）。このセクションの
	// スキーマは動的な options を持たないため untrack が無くても警告は出ないが、
	// 他の2セクションと同じ書き方に揃えている。
	let createConnectionStore = $state(untrack(() => createFormStore(connectionSchema(PLC_CREATE))));
	let creatingConnection = $state(false);

	async function handleCreateConnection(values: Record<string, unknown>): Promise<void> {
		creatingConnection = true;
		try {
			await createPlcConnection(toPlcConnectionInput(PLC_CREATE, values));
			toastStore.push('success', '作成しました');
			createConnectionStore = createFormStore(connectionSchema(PLC_CREATE));
			await reloadConnections();
		} catch (err) {
			applyServerErrors(PLC_CREATE, CONNECTION_WIRE_FIELDS, err, createConnectionStore);
		} finally {
			creatingConnection = false;
		}
	}

	const connectionColumns: GridColumn<PlcConnection>[] = [
		{ id: 'id', header: 'ID', accessor: 'id', width: 60, align: 'right' },
		{
			id: 'name',
			header: '名前',
			accessor: 'name',
			width: 160,
			filterable: true,
			filterType: 'text'
		},
		{ id: 'protocol', header: 'プロトコル', accessor: 'protocol', width: 110 },
		{ id: 'host', header: 'ホスト', accessor: 'host', width: 140 },
		{ id: 'port', header: 'ポート', accessor: 'port', width: 80, align: 'right' },
		{ id: 'unitId', header: 'ユニットID', accessor: 'unitId', width: 90, align: 'right' },
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		}
	];

	let selectedConnection: PlcConnection | null = $state(null);
	let editConnectionStore = $state(untrack(() => createFormStore(connectionSchema(PLC_EDIT))));
	let savingConnection = $state(false);
	let deletingConnection = $state(false);

	/**
	 * #391 レビュー A / #394 追補: 保存中・削除中は選択行を切り替えさせない。
	 * 1文字だけのガードでは不十分な理由（応答側の照合も不変条件として持つ
	 * 理由）は`runGuardedSave`/`runGuardedDelete`（`tagsPageLogic.ts`）の
	 * doc comment参照。
	 */
	function selectConnection(conn: PlcConnection): void {
		if (!canWrite || savingConnection || deletingConnection) return;
		selectedConnection = conn;
		editConnectionStore = createFormStore(
			connectionSchema(PLC_EDIT),
			connectionFormValues(PLC_EDIT, conn)
		);
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
		const pending: SaveGuardToken<typeof editConnectionStore> = {
			id: selectedConnection.id,
			store: editConnectionStore
		};
		savingConnection = true;
		try {
			const outcome = await runGuardedSave(
				pending,
				updatePlcConnection(
					selectedConnection.id,
					toPlcConnectionInput(PLC_EDIT, editConnectionStore.values)
				),
				() => ({ id: selectedConnection?.id, store: editConnectionStore })
			);
			switch (outcome.kind) {
				case 'applied':
					toastStore.push('success', '更新しました');
					selectedConnection = outcome.entity;
					await reloadConnections();
					break;
				case 'stale-success':
					// 応答が返ってきた時点で既に別の接続を選び直している - 一覧の
					// 鮮度だけは保つが、今開いているフォームには適用しない。
					await reloadConnections();
					break;
				case 'error':
					applyServerErrors(PLC_EDIT, CONNECTION_WIRE_FIELDS, outcome.err, editConnectionStore);
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
		ALLOWED_PERIOD_MS.map((ms) => ({ value: ms, label: ms >= 1000 ? `${ms / 1000}s` : `${ms}ms` }))
	);
	const connectionOptions = $derived(
		connections.map((c) => ({ value: c.id, label: `${c.name}（${c.protocol}）` }))
	);

	function groupSchema(prefix: string): FormSchema {
		return {
			fields: [
				{ name: `${prefix}Name`, label: '名前', type: 'text', required: true, min: 1, max: 100 },
				{
					name: `${prefix}PlcConnectionId`,
					label: 'PLC接続',
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

	let groups: CollectionGroup[] = $state([]);
	let groupsLoading = $state(false);

	async function reloadGroups(): Promise<void> {
		if (!available) return;
		groupsLoading = true;
		try {
			groups = await listCollectionGroups();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			groupsLoading = false;
		}
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
			await createCollectionGroup(toCollectionGroupInput(GROUP_CREATE, values));
			toastStore.push('success', '作成しました');
			createGroupStore = createFormStore(groupSchema(GROUP_CREATE));
			await reloadGroups();
		} catch (err) {
			applyServerErrors(GROUP_CREATE, GROUP_WIRE_FIELDS, err, createGroupStore);
		} finally {
			creatingGroup = false;
		}
	}

	function connectionName(id: number): string {
		return connections.find((c) => c.id === id)?.name ?? `#${id}`;
	}

	const groupColumns: GridColumn<CollectionGroup>[] = [
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
			id: 'plcConnectionId',
			header: 'PLC接続',
			accessor: (row) => connectionName(row.plcConnectionId),
			width: 160
		},
		{
			id: 'periodMs',
			header: '収集周期',
			accessor: 'periodMs',
			width: 100,
			align: 'right',
			format: (v) => (typeof v === 'number' && v >= 1000 ? `${v / 1000}s` : `${v}ms`)
		},
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		}
	];

	let selectedGroup: CollectionGroup | null = $state(null);
	let editGroupStore = $state(untrack(() => createFormStore(groupSchema(GROUP_EDIT))));
	let savingGroup = $state(false);
	let deletingGroup = $state(false);

	/** #391 レビュー A / #394 追補: 保存中・削除中は選択行を切り替えさせない（理由は `selectConnection` 参照）。 */
	function selectGroup(group: CollectionGroup): void {
		if (!canWrite || savingGroup || deletingGroup) return;
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
				case 'applied':
					toastStore.push('success', '削除しました');
					selectedGroup = null;
					await reloadGroups();
					break;
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

	const dataTypeOptions: { value: TagDataType; label: string }[] = [
		{ value: 'bit', label: 'ビット' },
		{ value: 'i16', label: '16bit（符号あり）' },
		{ value: 'u16', label: '16bit（符号なし）' },
		{ value: 'i32', label: '32bit（符号あり）' },
		{ value: 'u32', label: '32bit（符号なし）' },
		{ value: 'f32', label: '32bit実数' },
		{ value: 'i64', label: '64bit（符号あり、Modbusのみ）' },
		{ value: 'u64', label: '64bit（符号なし、Modbusのみ）' },
		{ value: 'f64', label: '64bit実数（倍精度、Modbusのみ）' }
	];

	const groupOptions = $derived(groups.map((g) => ({ value: g.id, label: g.name })));

	function tagSchema(prefix: string): FormSchema {
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
					label: 'デバイスアドレス',
					type: 'text',
					required: true,
					placeholder: 'D3000 / 40001 など'
				},
				{
					name: `${prefix}DataType`,
					label: 'データ型',
					type: 'select',
					required: true,
					default: 'i16',
					options: dataTypeOptions
				},
				{ name: `${prefix}RawLo`, label: 'スケーリング: 生値 下限', type: 'number' },
				{ name: `${prefix}RawHi`, label: 'スケーリング: 生値 上限', type: 'number' },
				{ name: `${prefix}EngLo`, label: 'スケーリング: 工学値 下限', type: 'number' },
				{ name: `${prefix}EngHi`, label: 'スケーリング: 工学値 上限', type: 'number' },
				{ name: `${prefix}Unit`, label: '単位', type: 'text' },
				// min/max は banto_tags::tag の MIN_DECIMALS/MAX_DECIMALS と一致
				// させる（クライアント側とサーバー側の検証範囲がずれると、
				// クライアントを通った値がサーバーで人間可読エラーとして
				// 跳ね返るだけの手戻りになる）。
				{ name: `${prefix}Decimals`, label: '小数桁', type: 'number', min: 0, max: 6, default: 0 },
				{ name: `${prefix}Enabled`, label: '有効', type: 'checkbox', default: true }
			]
		};
	}

	function toTagInput(prefix: string, values: Record<string, unknown>): TagInput {
		const numOrNull = (v: unknown): number | null => (typeof v === 'number' ? v : null);
		const rawUnit = values[`${prefix}Unit`];
		const unit = typeof rawUnit === 'string' ? rawUnit.trim() : '';
		const collectionGroupId = values[`${prefix}CollectionGroupId`];
		const decimals = values[`${prefix}Decimals`];
		return {
			name: String(values[`${prefix}Name`] ?? ''),
			collectionGroupId: typeof collectionGroupId === 'number' ? collectionGroupId : 0,
			address: String(values[`${prefix}Address`] ?? ''),
			dataType: (values[`${prefix}DataType`] as TagDataType) ?? 'i16',
			rawLo: numOrNull(values[`${prefix}RawLo`]),
			rawHi: numOrNull(values[`${prefix}RawHi`]),
			engLo: numOrNull(values[`${prefix}EngLo`]),
			engHi: numOrNull(values[`${prefix}EngHi`]),
			unit: unit === '' ? null : unit,
			decimals: typeof decimals === 'number' ? decimals : 0,
			enabled: Boolean(values[`${prefix}Enabled`])
		};
	}

	function tagFormValues(prefix: string, tag: Tag): Record<string, unknown> {
		return {
			[`${prefix}Name`]: tag.name,
			[`${prefix}CollectionGroupId`]: tag.collectionGroupId,
			[`${prefix}Address`]: tag.address,
			[`${prefix}DataType`]: tag.dataType,
			[`${prefix}RawLo`]: tag.rawLo,
			[`${prefix}RawHi`]: tag.rawHi,
			[`${prefix}EngLo`]: tag.engLo,
			[`${prefix}EngHi`]: tag.engHi,
			[`${prefix}Unit`]: tag.unit,
			[`${prefix}Decimals`]: tag.decimals,
			[`${prefix}Enabled`]: tag.enabled
		};
	}

	// `CONNECTION_WIRE_FIELDS`と同じ理由。
	const TAG_WIRE_FIELDS = schemaWireFields(TAG_CREATE, tagSchema(TAG_CREATE).fields);

	let tags: Tag[] = $state([]);
	let tagsLoading = $state(false);

	async function reloadTags(): Promise<void> {
		if (!available) return;
		tagsLoading = true;
		try {
			tags = await listTags();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			tagsLoading = false;
		}
	}

	let createTagStore = $state(untrack(() => createFormStore(tagSchema(TAG_CREATE))));
	let creatingTag = $state(false);

	async function handleCreateTag(values: Record<string, unknown>): Promise<void> {
		creatingTag = true;
		try {
			await createTag(toTagInput(TAG_CREATE, values));
			toastStore.push('success', '作成しました');
			createTagStore = createFormStore(tagSchema(TAG_CREATE));
			await reloadTags();
		} catch (err) {
			applyServerErrors(TAG_CREATE, TAG_WIRE_FIELDS, err, createTagStore);
		} finally {
			creatingTag = false;
		}
	}

	function groupName(id: number): string {
		return groups.find((g) => g.id === id)?.name ?? `#${id}`;
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

	/** #391 レビュー A / #394 追補: 保存中・削除中は選択行を切り替えさせない（理由は `selectConnection` 参照）。 */
	function selectTag(tag: Tag): void {
		if (!canWrite || savingTag || deletingTag) return;
		selectedTag = tag;
		editTagStore = createFormStore(tagSchema(TAG_EDIT), tagFormValues(TAG_EDIT, tag));
	}

	/** #394 レビュー（追補2）: 削除中も保存を素通しさせない（理由は `saveConnection` 参照）。 */
	async function saveTag(): Promise<void> {
		if (!selectedTag || savingTag || deletingTag) return;
		if (!editTagStore.validateAll()) return;
		const pending: SaveGuardToken<typeof editTagStore> = {
			id: selectedTag.id,
			store: editTagStore
		};
		savingTag = true;
		try {
			const outcome = await runGuardedSave(
				pending,
				updateTag(selectedTag.id, toTagInput(TAG_EDIT, editTagStore.values)),
				() => ({ id: selectedTag?.id, store: editTagStore })
			);
			switch (outcome.kind) {
				case 'applied':
					toastStore.push('success', '更新しました');
					selectedTag = outcome.entity;
					await reloadTags();
					break;
				case 'stale-success':
					await reloadTags();
					break;
				case 'error':
					applyServerErrors(TAG_EDIT, TAG_WIRE_FIELDS, outcome.err, editTagStore);
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
		if (!selectedTag || savingTag || deletingTag) return;
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
					selectedTag = null;
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

	$effect(() => {
		void reloadConnections();
		void reloadGroups();
		void reloadTags();
	});
</script>

<div class="page">
	<h2>タグ設定</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはレジストリDBが無いため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<section class="registry-section">
			<h3>PLC接続</h3>
			{#if canWrite}
				<div class="create">
					<h4>新規作成</h4>
					<BantoForm
						schema={connectionSchema(PLC_CREATE)}
						store={createConnectionStore}
						onSubmit={handleCreateConnection}
						submitting={creatingConnection}
						submitLabel="作成"
					/>
				</div>
			{/if}

			<div class="list">
				<p class="note">
					{canWrite
						? '行をクリックすると下に編集パネルが表示されます。'
						: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
				</p>
				{#if connectionsLoading && connections.length === 0}
					<p class="loading">読み込み中…</p>
				{:else}
					<!-- #391 レビュー A / #394 追補: 保存中・削除中は行の選択を操作
					できないようにする（`selectConnection`自体のガードに加え、
					見た目でも伝える）。 -->
					<div
						class="grid-wrap"
						class:saving={savingConnection || deletingConnection}
						aria-disabled={savingConnection || deletingConnection}
					>
						<BantoGrid
							rows={connections}
							columns={connectionColumns}
							getRowId={(c) => c.id}
							onRowClick={canWrite ? selectConnection : undefined}
						/>
					</div>
				{/if}
			</div>

			{#if selectedConnection && canWrite}
				<div class="detail">
					<h4>{selectedConnection.name} を編集</h4>
					<BantoForm
						schema={connectionSchema(PLC_EDIT)}
						store={editConnectionStore}
						onSubmit={saveConnection}
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
				</div>
			{/if}
		</section>

		<section class="registry-section">
			<h3>収集グループ</h3>
			{#if canWrite}
				<div class="create">
					<h4>新規作成</h4>
					{#if connections.length === 0}
						<p class="note">先にPLC接続を1件以上作成してください。</p>
					{:else}
						<BantoForm
							schema={groupSchema(GROUP_CREATE)}
							store={createGroupStore}
							onSubmit={handleCreateGroup}
							submitting={creatingGroup}
							submitLabel="作成"
						/>
					{/if}
				</div>
			{/if}

			<div class="list">
				<p class="note">
					{canWrite
						? '行をクリックすると下に編集パネルが表示されます。'
						: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
				</p>
				{#if groupsLoading && groups.length === 0}
					<p class="loading">読み込み中…</p>
				{:else}
					<div
						class="grid-wrap"
						class:saving={savingGroup || deletingGroup}
						aria-disabled={savingGroup || deletingGroup}
					>
						<BantoGrid
							rows={groups}
							columns={groupColumns}
							getRowId={(g) => g.id}
							onRowClick={canWrite ? selectGroup : undefined}
						/>
					</div>
				{/if}
			</div>

			{#if selectedGroup && canWrite}
				<div class="detail">
					<h4>{selectedGroup.name} を編集</h4>
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
				</div>
			{/if}
		</section>

		<section class="registry-section">
			<h3>タグ</h3>
			{#if canWrite}
				<div class="create">
					<h4>新規作成</h4>
					{#if groups.length === 0}
						<p class="note">先に収集グループを1件以上作成してください。</p>
					{:else}
						<BantoForm
							schema={tagSchema(TAG_CREATE)}
							store={createTagStore}
							onSubmit={handleCreateTag}
							submitting={creatingTag}
							submitLabel="作成"
						/>
					{/if}
				</div>
			{/if}

			<div class="list">
				<p class="note">
					{canWrite
						? '行をクリックすると下に編集パネルが表示されます。'
						: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
				</p>
				{#if tagsLoading && tags.length === 0}
					<p class="loading">読み込み中…</p>
				{:else}
					<div
						class="grid-wrap"
						class:saving={savingTag || deletingTag}
						aria-disabled={savingTag || deletingTag}
					>
						<BantoGrid
							rows={tags}
							columns={tagColumns}
							getRowId={(t) => t.id}
							onRowClick={canWrite ? selectTag : undefined}
						/>
					</div>
				{/if}
			</div>

			{#if selectedTag && canWrite}
				<div class="detail">
					<h4>{selectedTag.name} を編集</h4>
					<BantoForm
						schema={tagSchema(TAG_EDIT)}
						store={editTagStore}
						onSubmit={saveTag}
						submitting={savingTag || deletingTag}
						submitLabel="保存"
					>
						<button
							type="button"
							class="danger"
							onclick={handleDeleteTag}
							disabled={savingTag || deletingTag}
						>
							削除
						</button>
					</BantoForm>
				</div>
			{/if}
		</section>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		max-width: 860px;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.registry-section {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
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

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.grid-wrap {
		height: 260px;
	}

	.create,
	.detail {
		border-top: 1px solid var(--banto-border);
		padding-top: 0.75rem;
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
