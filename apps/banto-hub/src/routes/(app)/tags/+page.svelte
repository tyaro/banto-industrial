<script lang="ts">
	/**
	 * タグ登録（tags）CRUD 画面。plc-connections/collection-groups と同じ
	 * シンプル型を反復した新規作成（実装指示: 「tags 画面は 1737 行版
	 * （一括/連続登録込み）をコピーしない」）。
	 *
	 * T13-1（2026-08-08、docs/ux-plan.md §4b）: master-detail レイアウトへ
	 * 刷新。左は ConnectionTree（接続→収集グループの2階層、汎用部品
	 * TreeView.svelte を流し込むアプリ側コンポーネント）、右はツールバー
	 * + 画面全高の BantoGrid。フォームは通常登録・行クリック編集・連続
	 * 登録・CSVインポートの4フローすべてを Drawer（汎用部品、右スライド
	 * オーバー）に収める。フォームの状態管理・検証・dry-run フローの
	 * ロジックは旧インライン版から変更していない（`drawerMode` が
	 * 旧 `mode`/`selected` の可視状態を統合しただけ）。
	 *
	 * フォーム項目は `TagInput`（tagRegistryAdmin.ts、
	 * `banto_hub_core::rest::TagPayload` と同型）に1:1対応する。数値項目は
	 * すべて文字列で保持し、空欄 = 未設定（`toInput` で `undefined` に
	 * 変換 - JSON.stringify がフィールド自体を省略し、バックエンドの
	 * `#[serde(default)]` が `None` として受け取る）。`stringLength` は
	 * `dataType === 'string'` のときのみ表示・送信する（`MIN_STRING_LENGTH`/
	 * `MAX_STRING_LENGTH` はヒント表示のみ - 実際の検証はバックエンド）。
	 */
	import { tick } from 'svelte';
	import { page } from '$app/state';
	import { BantoGrid, type CellEdit, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import Drawer from '$lib/components/Drawer.svelte';
	import Modal from '$lib/components/Modal.svelte';
	import SplitPane from '$lib/components/SplitPane.svelte';
	import ConnectionTree from '$lib/components/ConnectionTree.svelte';
	import TreeContextMenu from '$lib/components/TreeContextMenu.svelte';
	import ConnectionDrawer from '$lib/components/ConnectionDrawer.svelte';
	import CollectionGroupDrawer from '$lib/components/CollectionGroupDrawer.svelte';
	import type { ConnectionTreeNodeData } from '$lib/components/connectionTreeTypes';
	import type { TreeNode } from '$lib/components/treeTypes';
	import {
		listTags,
		createTag,
		updateTag,
		deleteTag,
		listCollectionGroups,
		listPlcConnections,
		createTagsBatch,
		updateTagsBatch,
		isTagRevisionConflictError,
		MIN_STRING_LENGTH,
		MAX_STRING_LENGTH,
		MIN_DECIMALS,
		MAX_DECIMALS,
		TAG_KIND_OPTIONS,
		CALC_CONNECTION_NAME,
		MEM_CONNECTION_NAME,
		type Tag,
		type TagInput,
		type TagDataType,
		type TagKind,
		type CollectionGroup,
		type PlcConnection,
		type BatchTagsResult,
		type BatchTagsUpdateResult,
		type BatchTagUpdateRow
	} from '$lib/banto/tagRegistryAdmin';
	import {
		buildBulkEnableRows,
		buildBulkMoveRows,
		hasMixedTagKinds,
		summarizeBulkChange,
		type BulkChangeSummary
	} from '$lib/banto/tagBulkOps';
	import {
		applyTagCellOverrides,
		buildTagCellEditBatch,
		mergeTagCellEdits,
		type EditableTagField,
		type TagCellEditInput
	} from '$lib/banto/tagCellEdit';
	import { getHubStatus, type StatusResponse } from '$lib/banto/hubStatus';
	import {
		buildContinuousParams,
		generateContinuousTags,
		MAX_CONTINUOUS_COUNT,
		nextNamePatternOnAddressChange,
		nextStartNumberOnAddressChange,
		type ContinuousFormState,
		type ContinuousRegistrationResult
	} from '$lib/banto/continuousRegistration';
	import {
		exportTagsCsv,
		parseTagsCsv,
		parseCsv,
		stripBom,
		buildTagCsvTemplate,
		buildErrorRowsCsv,
		checkCsvSizeLimit,
		checkCsvRowLimit,
		type ImportTagsCsvResult,
		type ParsedCsvTagRow,
		type CsvRowError
	} from '$lib/banto/tagCsv';
	import {
		classifyCsvUpdate,
		type CsvUpdateClassification,
		type CsvUpdateRow,
		type CsvRowCategory
	} from '$lib/banto/tagCsvDiff';
	import { parseOptionalNumber } from '$lib/banto/tagFormNumeric';
	import {
		DISPLAY_SCALING_FIELDS,
		DISPLAY_SCALING_VALUE_FIELDS,
		THRESHOLD_FIELDS,
		WRITE_SAFETY_FIELDS,
		hasFieldError,
		hasAnyFieldValue,
		buildConfirmExternalName,
		environmentLabel,
		writePermissionLabel,
		fieldErrorsFromList
	} from '$lib/banto/tagFormLayout';
	import { addressHelpFor } from '$lib/banto/tagAddressHelp';
	import { carryFormForNext } from '$lib/banto/tagFormCarry';
	import { buildDuplicateFormValues } from '$lib/banto/tagDuplicate';
	import { nextTagNameOnAddressChange } from '$lib/banto/tagNamePrefill';
	import { canDefaultWritable, writableDefaultBlockedReason } from '$lib/banto/writableDefault';
	import {
		monitorHref,
		resolveGroupIdFromTreeSelection,
		resolvePresetGroupId,
		type TreeSelectionForPreset
	} from '$lib/banto/tagOnboarding';
	import { isFormDirty } from '$lib/banto/formDirty';
	import {
		buildExternalName,
		findReferencingComputedTags,
		formatDeleteConfirmMessage
	} from '$lib/banto/tagDeleteImpact';
	import { diffFormRecords, type ConflictFieldDiff } from '$lib/banto/tagConflictDiff';
	import {
		resolveTreeContextMenuItemsForRole,
		type TreeContextMenuItemAction
	} from '$lib/banto/tagTreeContextMenu';
	import { beforeNavigate } from '$app/navigation';

	const dataTypeOptions: { value: TagDataType; label: string }[] = [
		{ value: 'bit', label: 'bit（真偽値1点）' },
		{ value: 'i16', label: 'i16（符号あり16bit）' },
		{ value: 'u16', label: 'u16（符号なし16bit）' },
		{ value: 'i32', label: 'i32（符号あり32bit）' },
		{ value: 'u32', label: 'u32（符号なし32bit）' },
		{ value: 'f32', label: 'f32（浮動小数点32bit）' },
		{ value: 'string', label: 'string（文字列）' }
	];

	/**
	 * T18-5a（docs/banto-hub-t18-design.md「T18-5a 大量タグ性能」第1段）:
	 * 一括登録/更新のプレビュー表（連続登録・CSV新規・CSV更新差分・一括操作
	 * 差分の4箇所）は最大 MAX_CSV_ROWS（10,000行）まで全件を DOM 描画して
	 * いたため、大規模 CSV では描画が重くなる。検証（dryRun）・適用
	 * （createTagsBatch/updateTagsBatch 等）・件数サマリ・エラー一覧は
	 * 引き続き全件を対象にしたまま、`{#each}` に渡す配列だけ先頭
	 * PREVIEW_DISPLAY_LIMIT 件に絞る（サーバーページング/windowed grid化
	 * する第2段は別途対応）。
	 *
	 * 上限は連続登録の最大点数 MAX_CONTINUOUS_COUNT（=1000）に合わせる:
	 * 連続登録プレビュー（最大1000行）は全件表示のままにし（1000行は許容
	 * 範囲で、`banto-hub-tags-continuous.spec.ts` の「点数1000 →
	 * プレビュー1000件」も維持される）、真に重い CSV インポート
	 * （最大 MAX_CSV_ROWS=10000 行）だけが上限化される（10000→1000 で
	 * DOM 描画を1桁削減）。
	 */
	const PREVIEW_DISPLAY_LIMIT = 1000;

	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	interface FormState {
		name: string;
		collectionGroupId: string;
		address: string;
		dataType: TagDataType;
		stringLength: string;
		rawLo: string;
		rawHi: string;
		engLo: string;
		engHi: string;
		unit: string;
		decimals: string;
		thresholdH: string;
		thresholdHh: string;
		thresholdL: string;
		thresholdLl: string;
		enabled: boolean;
		/**
		 * 書き込み可（T2-3、docs/tag-server-design.md §6 item 1）。既定 off -
		 * 明示的に opt-in させたタグだけを書き込み対象にする設計（per-tag
		 * opt-in）に合わせる。`computed` タグではこのチェックボックス自体を
		 * 隠す（送信時に強制 false - サーバー側も writable=true を拒否する）。
		 */
		writable: boolean;
		/** T6-2: タグ種別。既定は既存どおり `plc`。 */
		tagKind: TagKind;
		/** T6-2: 演算タグの式ソース（`tagKind === 'computed'` のときのみ表示・送信）。 */
		expression: string;
		/** T6-2: 内部タグの再起動時復元フラグ（`tagKind === 'internal'` のときのみ表示）。 */
		retain: boolean;
	}

	function blankForm(): FormState {
		return {
			name: '',
			collectionGroupId: '',
			address: '',
			dataType: 'f32',
			stringLength: '',
			rawLo: '',
			rawHi: '',
			engLo: '',
			engHi: '',
			unit: '',
			decimals: '0',
			thresholdH: '',
			thresholdHh: '',
			thresholdL: '',
			thresholdLl: '',
			enabled: true,
			writable: false,
			tagKind: 'plc',
			expression: '',
			retain: false
		};
	}

	function numOrEmpty(v: number | null): string {
		return v === null ? '' : String(v);
	}

	function formFromTag(t: Tag): FormState {
		return {
			name: t.name,
			collectionGroupId: String(t.collectionGroupId),
			address: t.address,
			dataType: t.dataType,
			stringLength: numOrEmpty(t.stringLength),
			rawLo: numOrEmpty(t.rawLo),
			rawHi: numOrEmpty(t.rawHi),
			engLo: numOrEmpty(t.engLo),
			engHi: numOrEmpty(t.engHi),
			unit: t.unit ?? '',
			decimals: String(t.decimals),
			thresholdH: numOrEmpty(t.thresholdH),
			thresholdHh: numOrEmpty(t.thresholdHh),
			thresholdL: numOrEmpty(t.thresholdL),
			thresholdLl: numOrEmpty(t.thresholdLl),
			enabled: t.enabled,
			writable: t.writable,
			tagKind: t.tagKind,
			expression: t.expression ?? '',
			retain: t.retain
		};
	}

	/**
	 * T18-1（TAG-UX-C 4点目「差分表示 UI」）: revision 競合パネルの表に出す
	 * フィールドラベル（日本語）。`diffFormRecords`（`$lib/banto/tagConflictDiff.ts`）
	 * 自体は `FormState` のキー名を知らない汎用ヘルパーのため、ラベルマップは
	 * このページ側で持つ。
	 */
	const FIELD_LABELS: Record<string, string> = {
		name: '名前',
		collectionGroupId: '収集グループ',
		address: 'アドレス',
		dataType: 'データ型',
		stringLength: '文字列長',
		rawLo: 'RawLo',
		rawHi: 'RawHi',
		engLo: 'EngLo',
		engHi: 'EngHi',
		unit: '単位',
		decimals: '小数桁数',
		thresholdH: 'しきい値 H',
		thresholdHh: 'しきい値 HH',
		thresholdL: 'しきい値 L',
		thresholdLl: 'しきい値 LL',
		enabled: '有効',
		writable: '書き込み可',
		tagKind: 'タグ種別',
		expression: '式（expression）',
		retain: 'retain'
	};

	/**
	 * 差分表示用に `FormState` を plain object へ変換する — `collectionGroupId`
	 * は数値 ID のままだと差分パネルで読みにくいため、グループ名に変換して
	 * 渡す（`diffFormRecords` はキーの意味を知らない汎用比較のため、この
	 * 変換は呼び出し側の責務）。
	 */
	function conflictRecord(form: FormState): Record<string, unknown> {
		return { ...form, collectionGroupId: groupName(Number(form.collectionGroupId)) };
	}

	/**
	 * T6-2 (docs/tag-server-design.md §4.2's table): `computed`/`internal`
	 * tags carry no PLC address at all — send an empty string regardless of
	 * whatever the (hidden) address field still holds from a prior `plc`
	 * selection, so switching `tagKind` in the form can never leak a stale
	 * address into a payload the backend would reject.
	 */
	function toInput(form: FormState): TagInput {
		const isPlc = form.tagKind === 'plc';
		const isComputed = form.tagKind === 'computed';
		return {
			name: form.name,
			collectionGroupId: Number(form.collectionGroupId),
			address: isPlc ? form.address : '',
			dataType: form.dataType,
			stringLength: form.dataType === 'string' ? parseOptionalNumber(form.stringLength) : undefined,
			rawLo: parseOptionalNumber(form.rawLo),
			rawHi: parseOptionalNumber(form.rawHi),
			engLo: parseOptionalNumber(form.engLo),
			engHi: parseOptionalNumber(form.engHi),
			unit: form.unit === '' ? undefined : form.unit,
			decimals: Number(form.decimals),
			thresholdH: parseOptionalNumber(form.thresholdH),
			thresholdHh: parseOptionalNumber(form.thresholdHh),
			thresholdL: parseOptionalNumber(form.thresholdL),
			thresholdLl: parseOptionalNumber(form.thresholdLl),
			enabled: form.enabled,
			// computed タグは常に writable=false（値は式が決める、§4.2表）-
			// フォーム自体もこのチェックボックスを隠すが、送信直前にも強制する。
			writable: isComputed ? false : form.writable,
			tagKind: form.tagKind,
			expression: isComputed ? form.expression : undefined,
			retain: form.tagKind === 'internal' ? form.retain : false
		};
	}

	/**
	 * T18-2a（TAG-UX-B「保存前に最終外部名 `{connection}.{group}.{tag}`、
	 * 実機／SIM、書き込み許可を固定領域で確認できるようにする」）:
	 * `tagFields` スニペット末尾の「保存前の確認」領域（`.confirm-panel`）
	 * を駆動する3つの薄いラッパー。実際の組み立てロジックは
	 * `$lib/banto/tagFormLayout.ts` の純関数へ切り出してあり、ここは
	 * `groups`/`connections`（このページの `$state`）からフォームの
	 * `collectionGroupId` に対応する接続・グループを引くだけの責務。
	 */
	function connectionForGroupId(id: string): PlcConnection | undefined {
		const gid = Number(id);
		if (!Number.isFinite(gid)) return undefined;
		const group = groups.find((g) => g.id === gid);
		return group ? connections.find((c) => c.id === group.plcConnectionId) : undefined;
	}

	function confirmExternalName(form: FormState): string {
		const conn = connectionForGroupId(form.collectionGroupId);
		const group = groups.find((g) => String(g.id) === form.collectionGroupId);
		return buildConfirmExternalName({
			connectionName: conn?.name,
			groupName: group?.name,
			tagName: form.name
		});
	}

	function confirmEnvironmentLabel(form: FormState): string {
		return environmentLabel(connectionForGroupId(form.collectionGroupId)?.simulation);
	}

	function confirmWriteLabel(form: FormState): string {
		return writePermissionLabel(form.tagKind, form.writable);
	}

	/**
	 * T18-2a（TAG-UX-G「必須項目、ヒント、エラーを `required` /
	 * `aria-invalid` / `aria-describedby` で関連付け」）: 複数の id
	 * （ヒント span・エラー span）を空白区切りの `aria-describedby` へ
	 * まとめる。`false`/`undefined` は素通りさせて呼び出し側が
	 * 三項演算子を書かずに済むようにする — 対象が無ければ属性自体を
	 * 付けない（`undefined` を返す）。
	 */
	function describedBy(...ids: (string | false | undefined)[]): string | undefined {
		const list = ids.filter((id): id is string => Boolean(id));
		return list.length > 0 ? list.join(' ') : undefined;
	}

	/**
	 * T13-1 (docs/ux-plan.md §4b): 通常登録・行クリック編集・連続登録・
	 * CSVインポートの4フローを1つの Drawer に集約する。旧 `Mode`
	 * （'single' | 'continuous' | 'csv' の表示切替）と旧「選択中タグの
	 * 編集パネル常時表示」を、この `drawerMode` 1つに統合した -
	 * `drawerMode !== null` が Drawer の `open` を駆動する。
	 */
	type DrawerMode = 'create' | 'edit' | 'continuous' | 'csv' | null;
	let drawerMode: DrawerMode = $state(null);

	let groups: CollectionGroup[] = $state([]);
	let connections: PlcConnection[] = $state([]);
	let tags: Tag[] = $state([]);
	let loading = $state(false);
	/**
	 * T18-1（TAG-UX-C 6点目、docs/banto-hub-desktop-plan.md §9.4）:
	 * 初期読込失敗・再読込失敗を通信エラーとして保持する - `tags` は
	 * 失敗時も直前の内容を残す（stale 維持、`monitor/+page.svelte` の
	 * `loadError` と同じパターン）。「通信失敗を『タグ0件』と表示しない」
	 * ため、`tags.length === 0` だけでは空とみなさず、この値も併せて見る。
	 * 成功時にのみ `null` に戻す。
	 */
	let loadError = $state<string | null>(null);

	/**
	 * T18-3e（docs/banto-hub-t18-design.md「T18-3e BantoGrid セル編集/TSV貼付
	 * の接続」、実装指示「停止中のみ編集可（停止中ロック）」）: BantoGrid の
	 * セル編集/TSV貼付は収集停止中（`collection_state === 'stopped'`）のみ
	 * 許可する。`getHubStatus()`（`GET /api/status`、2026-08-31 オーナー
	 * 決定で `/api/v1/status` から切替 - `hubStatus.ts`参照）は初期ロード時と
	 * 保存直前（`handleSaveGridEdits`）の両方で呼び直す - 収集の開始/停止は
	 * 別画面から行われうるため、このページを開いたまま状態が変わる可能性が
	 * ある。取得に失敗した場合は `hubStatus` を `null` のままにし、
	 * `collectionStopped` は安全側（`false` = 編集不可）にフォールバックする。
	 */
	let hubStatus: StatusResponse | null = $state(null);
	const collectionStopped = $derived.by((): boolean => {
		const s = hubStatus;
		return s !== null && s.collection_state === 'stopped';
	});

	async function loadHubStatus(): Promise<void> {
		try {
			hubStatus = await getHubStatus();
		} catch {
			// 通信エラーは安全側（編集不可）にフォールバックするだけで、ここでは
			// トーストを出さない - `tags`/`groups`/`connections` の `reload()` が
			// 既に読込失敗バナーを持っており、これは表編集ロックの参考情報に
			// すぎないため二重にエラーを騒がしくしない。
			hubStatus = null;
		}
	}

	function groupName(id: number): string {
		return groups.find((g) => g.id === id)?.name ?? `#${id}`;
	}

	function connectionName(id: number): string | undefined {
		return connections.find((c) => c.id === id)?.name;
	}

	/**
	 * T6-2: groups の候補をタグ種別で絞り込む — `computed` は `calc` 接続
	 * 配下、`internal` は `mem` 接続配下、`plc` はそのどちらでもない接続配下
	 * のみ（`banto_tags::tag::validate_tag_kind_placement` と同じ規則）。
	 * サーバー側検証の先取りであって、これ自体が正の唯一の判定源ではない。
	 */
	function groupsFor(kind: TagKind): CollectionGroup[] {
		return groups.filter((g) => {
			const name = connectionName(g.plcConnectionId);
			if (kind === 'computed') return name === CALC_CONNECTION_NAME;
			if (kind === 'internal') return name === MEM_CONNECTION_NAME;
			return name !== CALC_CONNECTION_NAME && name !== MEM_CONNECTION_NAME;
		});
	}

	/**
	 * T18-1（TAG-UX-C 6点目）: 失敗しても `groups`/`connections`/`tags` を
	 * 差し替えない（stale 一覧を維持する） - 一度 `const` の配列に受けて
	 * から全部揃った時点でまとめて代入することで、途中失敗時に一部だけ
	 * 更新されて表示が食い違うことも防ぐ。
	 */
	async function reload(): Promise<void> {
		loading = true;
		try {
			const [nextGroups, nextConnections, nextTags] = await Promise.all([
				listCollectionGroups(),
				listPlcConnections(),
				listTags()
			]);
			groups = nextGroups;
			connections = nextConnections;
			tags = nextTags;
			loadError = null;
			// T18-3b: 再取得で消えた（削除された等の）タグの選択を掃除する -
			// 存在しない id を選択集合に残すと、一括操作の対象件数表示や
			// summarizeBulkChange の計算が古い/存在しない行を含んでしまう。
			if (selectedIds.size > 0) {
				const existingIds = new Set(nextTags.map((t) => t.id));
				const next = new Set([...selectedIds].filter((id) => existingIds.has(id)));
				if (next.size !== selectedIds.size) selectedIds = next;
			}
		} catch (err) {
			loadError = errorMessage(err);
			toastStore.push('error', loadError);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
		void loadHubStatus();
	});

	// --- create ---
	let createForm = $state(blankForm());
	/**
	 * T18-1（TAG-UX-C 一部）: create Drawer を開いた時点のスナップショット。
	 * `isFormDirty` で `createForm` と比較し、未保存変更の有無を判定する
	 * （リアクティブに参照しないので `$state` は不要 - 開いた時と保存成功
	 * 時にだけ差し替える）。
	 */
	let createBaseline: FormState = blankForm();
	let createErrors: Record<string, string> = $state({});
	let creating = $state(false);

	/**
	 * 2026-09-01 オーナー要望「タグ名が空欄のままならアドレスをタグ名として
	 * 使う」: create Drawer 専用の「名前欄をユーザーが直接編集したか」の
	 * 追跡フラグ。`ConnectionDrawer.svelte` の `portTouched` と同じ設計
	 * （`$lib/banto/tagNamePrefill.ts` モジュール doc comment 参照）。
	 *
	 * - `false`（既定）の間だけ、アドレス欄の入力に追従して名前欄を
	 *   プリフィルする（`nextTagNameOnAddressChange`）。
	 * - 名前欄の `oninput` で `true` に固定する（下の `tagFields` snippet
	 *   呼び出しの最終引数）。
	 * - 編集フォーム（`editForm`）は対象外のため、edit 側には対応する
	 *   touched 変数を持たない（`edit` 用の呼び出しは常に no-op を渡す）。
	 *
	 * リセットするタイミング:
	 * - `openCreateDrawer`（新規に空フォームを開く）: `false` に戻す。
	 * - `handleCreate` の「登録して次へ」（`carryFormForNext` で名前・
	 *   アドレスを空へ戻す）: 次の1件はまた「名前が空」から始まるので
	 *   `false` に戻す。
	 * - `openDuplicateDrawer`（タグ複製）: `buildDuplicateFormValues` が
	 *   既に意味のある複製名（`{元名}_copy` 等）を入れているため、これを
	 *   アドレス入力で上書きされたくない - あえて `true`（＝プリフィル
	 *   対象外）で開始する（`banto-hub-tags-duplicate.spec.ts` の「アドレス
	 *   を入れて登録しても複製名はそのまま」という既存挙動を壊さないための
	 *   判断）。
	 */
	let createNameTouched = $state(false);

	/**
	 * T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2・§3.3、2026-09-02
	 * オーナー決定「`writable` の既定 ON。ただし収集グループ単位で変更可
	 * （条件付き適用）」）: create Drawer 専用の「`writable` チェックボックス
	 * をユーザーが直接編集したか」の追跡フラグ。`createNameTouched` と同じ
	 * touched 追跡方式 - `false` の間だけ、下の `$effect`（タグ種別・
	 * アドレス・収集グループの変化を見る）が `createForm.writable` を
	 * 自動計算し続ける。チェックボックスをユーザーが直接クリックした時点で
	 * `true` に固定する（`tagFields` snippet の write-safety セクション参照）。
	 *
	 * リセットするタイミング:
	 * - `openCreateDrawer`: `false` に戻す（新規タグは常に自動計算から
	 *   始まる）。
	 * - `handleCreate` の「登録して次へ」: **触れない** - `carryFormForNext`
	 *   は `writable` を「直前の入力のまま引き継ぐ」共通値として扱っている
	 *   （`tagFormCarry.ts` 冒頭コメント参照）。touched もリセットすると、
	 *   ユーザーが前の1件で手動 OFF にした意図が次の1件で黙って ON に
	 *   戻ってしまう。
	 * - `openDuplicateDrawer`: **`true` で開始する**（`createNameTouched`
	 *   と同じ判断 - 複製元の `writable` は「型/単位/スケーリング/しきい値」
	 *   と同格の引き継ぎ対象であり、この既定計算で上書きすべきではない）。
	 */
	let createWritableTouched = $state(false);

	/**
	 * T19 S1-b（UX-34）: create Drawer が開いている間、`createForm.writable`
	 * を「PLC タグかどうか」（{@link canDefaultWritable}、
	 * `$lib/banto/writableDefault.ts`）とグループ単位の既定値
	 * （`CollectionGroup.defaultWritable` - 2026-09-02 オーナー判断
	 * 「グループ単位の既定値は DB 列に持つ」により、既に読み込み済みの
	 * `groups` 配列から直接引く。以前の実装は `localStorage` を使っていた
	 * が、本番投入前に安いうちに正しく持たせる判断でサーバー側の列へ
	 * 置き換えた）の両方から自動計算し続ける。`createWritableTouched` が
	 * `true` になった後は何もしない（ユーザーが自分で決めた値を上書き
	 * しない）。
	 *
	 * **2026-09-02 オーナー判断（S1-b0 分離）**: アドレス領域（Modbus
	 * `1xxxx`/`3xxxx` 読み取り専用）による絞り込みはここでは行わない -
	 * `canDefaultWritable` の第2引数（`writableArea`）を意図的に省略して
	 * いる（`undefined` 扱い）。この規則をアドレス文字列から UI 側で
	 * 判定すると、`banto-plc`（`AddressArea`）・`banto-tags`
	 * （`modbus_read_only_area`）に続く3つ目の手書き複製になってしまう
	 * ため、プロトコル層のデータを受け取れるようになる別スライス S1-b0
	 * まで保留する（`writableDefault.ts` の doc comment 参照）。S1-b0 が
	 * サーバー由来の判定結果（例: アドレス preflight のレスポンスに
	 * 判定結果を足す）を用意したら、ここへ第2引数として渡すだけで
	 * 絞り込みが有効になる - 関数シグネチャは既にその形になっている。
	 *
	 * 依存として読むのは `drawerMode`・`createWritableTouched`・
	 * `createForm.tagKind`・`createForm.collectionGroupId`・`groups` の5つ -
	 * `createForm.writable` 自身は読まない（読むと自分の書き込みで自分を
	 * 再トリガーする無限ループのリスクになる）。
	 */
	$effect(() => {
		if (drawerMode !== 'create' || createWritableTouched) return;
		const eligible = canDefaultWritable(createForm.tagKind);
		const groupId = Number(createForm.collectionGroupId);
		const selectedGroup =
			createForm.collectionGroupId !== '' && Number.isFinite(groupId)
				? groups.find((g) => g.id === groupId)
				: undefined;
		// 未選択・グループが見つからない間は全体既定の `true` にフォール
		// バックする（`groupWritableDefault.ts` の旧実装と同じ既定値）。
		const groupDefault = selectedGroup?.defaultWritable ?? true;
		createForm.writable = eligible && groupDefault;
	});

	/**
	 * T18-3a（docs/banto-hub-t18-design.md「T18-3a タグ複製」、TAG-UX-D
	 * 前半）: 複製元タグ。`openDuplicateDrawer` が set し、Drawer を閉じたら
	 * `closeDrawer` が `null` に戻す。`null` の間は create Drawer は通常の
	 * 新規作成（`openCreateDrawer` 経由）であり、複製元との差分パネルは
	 * 出さない。「登録して次へ」で複製フォームのまま続けて作成した場合も
	 * （`handleCreate` は `duplicateSource` に触れないため）そのまま複製元
	 * を保持し続ける - 直前の複製に対する差分として引き続き意味がある。
	 */
	let duplicateSource: Tag | null = $state(null);

	/**
	 * T18-3a（受け入れ「保存前に複製元との差分を確認できる」）: 複製元タグ
	 * （`duplicateSource`）と複製後フォーム（`createForm`）のフィールド単位
	 * 差分。`saveEdit` の revision 競合パネルが使っているのと同じ純関数
	 * `diffFormRecords`（`tagConflictDiff.ts`）をそのまま再利用する -
	 * 「サーバー最新 vs ローカル」を「複製元 vs 複製後」に読み替えただけで
	 * 意味は同じ（フィールド単位の value 差分）。`createForm` は `$state` の
	 * ため、入力するたびにこの `$derived` も再計算される（保存前に常に
	 * 最新の差分を確認できる）。
	 */
	const duplicateDiff = $derived.by((): ConflictFieldDiff[] | null => {
		if (drawerMode !== 'create' || !duplicateSource) return null;
		return diffFormRecords(
			conflictRecord(formFromTag(duplicateSource)),
			conflictRecord(createForm),
			FIELD_LABELS
		);
	});

	/**
	 * TAG-P0-2（docs/banto-hub-desktop-plan.md §9.3、2026-08-10 実装メモ）:
	 * バックエンドの preflight（`preflight_transaction` →
	 * `build_config_from`/`build_catalog_from`/`computed::build_plan`、
	 * `apps/banto-hub/core/src/rest.rs::preflight_api_error`）は
	 * アドレス解析・演算式・DAG 検証の失敗を単票フィールドではなく
	 * 常に `field: "configuration"` 1本にまとめて返す - `tagFields` は
	 * 個別フィールドの `errors.xxx` しか描画しないため、このマップに
	 * `configuration` を素通りさせただけではフォームのどこにも出ず、
	 * トーストも出ない（`applyFieldErrors` が返せば呼び出し元は
	 * `toastStore.push` をスキップする）ためサイレント失敗になっていた。
	 * `tagFields` 側に `errors.configuration` の全体エラー表示を追加した
	 * うえで、メッセージに「アドレス」を含む場合はアドレス欄の直下にも
	 * 同じ文言を出す（`errors.address` が未設定のときのみ - 将来
	 * アドレス欄自体のフィールドエラーが返るようになったら上書きしない）。
	 */
	function applyFieldErrors(err: unknown): Record<string, string> | null {
		if (isProviderError(err) && err.body.kind === 'validation') {
			return fieldErrorsFromList(err.body.field_errors);
		}
		return null;
	}

	/**
	 * T18-4c（docs/banto-hub-t18-design.md「T18-4c 確認導線」、
	 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-H「新規／変更タグを
	 * 『確認対象』として値・品質・時刻へ1クリックで移動できるようにする」）:
	 * T18-2d の `showMonitorCta`（新規作成専用の真偽値）を一般化し、新規・
	 * 複製・編集・連続登録・CSV 新規/更新取り込み・一括更新のすべての成功
	 * 経路で使う「次はここへ」リンク先に置き換えた。`null` はバナー非表示、
	 * 非 `null` なら `monitorHref()`（`tagOnboarding.ts`）が組み立てた
	 * `/monitor?...` を指す。`/monitor` は WS 経由の現在値を表示するのみで、
	 * ここから直接 SIM 値の good/bad は判定しない（判定はチェックリスト側
	 * `tagOnboarding.ts::computeOnboardingSteps` の責務のまま）。
	 *
	 * **QueuedWhileRunning（収集稼働中の 202 キュー投入）では設定しない**:
	 * 各ハンドラは `createTag`/`updateTag`/`createTagsBatch`/`updateTagsBatch`
	 * の `await` が成功した後（＝ `QueuedWhileRunningError` が投げられず
	 * catch に落ちなかった場合）にのみ `monitorCtaHref` を代入する。202
	 * 応答時は `tagRegistryAdmin.ts::httpRequest` がその `await` 自体を
	 * `QueuedWhileRunningError` として投げ、各ハンドラの `catch` ブロック
	 * （汎用エラートーストへ委ねている箇所）に落ちるため、この代入行へは
	 * 到達しない - 追加の分岐は不要で、既存の try/catch 構造がそのまま
	 * 「キュー投入時は CTA を出さない」を満たす。
	 */
	let monitorCtaHref: string | null = $state(null);

	/**
	 * T18-2c（docs/banto-hub-t18-design.md「T18-2c 登録後分岐と親引継ぎ」、
	 * TAG-UX-2）: create Drawer の主アクションを「登録して次へ」
	 * （`closeAfterSave = false`）と「登録して閉じる」（`true`）に分ける。
	 * `closeAfterSave` は create フォームの `onsubmit` が
	 * `SubmitEvent.submitter`（どちらのボタンが送信を起こしたか）から
	 * 決める - `submitter` が取れない場合（テキスト入力内での Enter
	 * 実装送信を `submitter` を返さない古いエンジンが処理した場合等）は
	 * `undefined` になり、`submitter?.id === 'create-register-close'` が
	 * 自然に `false`（＝「登録して次へ」側）へフォールバックする。これは
	 * DOM 上も「登録して次へ」ボタンを先に置くことで Enter 押下時の既定
	 * ボタンにしている（`banto-hub-tags-form.spec.ts`
	 * 1番の Enter 送信テストが前提にしている「保存後も Drawer が開いたまま」
	 * 挙動と一致させるため）。
	 *
	 * 「登録して閉じる」＝保存成功後に `closeDrawer()`（現状の Drawer
	 * `×`/Esc と同じ後始末）。「登録して次へ」＝保存成功後も Drawer は
	 * 開いたまま、`carryFormForNext`（`$lib/banto/tagFormCarry.ts`）で
	 * 名前・アドレスだけ空にした次フォームへ差し替える -
	 * タグ種別・収集グループ（「親設定」）を含むそれ以外のフィールドは
	 * すべて直前の入力のまま引き継ぐ（TAG-UX-2「親設定と明示選択した共通値を
	 * 保持」、具体的な選択 UI が設計書に無いため tagFormCarry.ts 冒頭の
	 * コメントに書いた既定にフォールバック）。`createBaseline` も同じ値へ
	 * 差し替えることで、引き継いだ値そのものは dirty 扱いにならない
	 * （名前・アドレスへ次の入力をしたときにだけ dirty になる）。
	 */
	async function handleCreate(closeAfterSave: boolean): Promise<void> {
		creating = true;
		createErrors = {};
		try {
			const created = await createTag(toInput(createForm));
			toastStore.push('success', '作成しました');
			monitorCtaHref = monitorHref({
				groupId: created.collectionGroupId,
				focus: [externalNameForTag(created)]
			});
			if (closeAfterSave) {
				closeDrawer();
			} else {
				const nextForm = carryFormForNext(createForm);
				createForm = nextForm;
				createBaseline = { ...nextForm };
				createAddressPreflight = blankAddressPreflight();
				// 2026-09-01: 名前・アドレスとも空へ戻る次の1件なので、また
				// 「名前が空」から始まる - プリフィル対象へリセットする。
				createNameTouched = false;
				// フォーム差し替え後の DOM 更新を待ってから、次の論理入力
				// （名前 - 全タグ種別で必須かつ常に空になる唯一のフィールド）
				// へフォーカスを移し、連続入力を続けられるようにする。
				await tick();
				document.getElementById('tag-name')?.focus();
			}
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) createErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			creating = false;
		}
	}

	/**
	 * T18-3b（docs/banto-hub-t18-design.md「T18-3b 一括操作」）: 複数選択
	 * された行の id 集合。BantoGrid（`node_modules/@banto/grid-svelte`）は
	 * 任意セル描画（チェックボックス列等）に対応しておらず、`editable` な
	 * 列を1つでも足すと `onRowClick` の意味が変わってしまう
	 * （編集済みでない列のシングルクリックが編集を開かなくなる - 既存の
	 * 「行クリックで編集パネルを開く」挙動を壊す）ため、選択列は追加せず
	 * `selectionMode`（下）でクリックの意味を「編集を開く」⇔「選択を
	 * 切り替える」で明示的に切り替える方式にした（実装指示の代案(b)）。
	 * 選択中の行は `rowClass`（下の `tagRowClass`）で強調表示する。
	 */
	let selectedIds = $state<Set<number>>(new Set());
	/** T18-3b: 選択モードのON/OFF。OFFへ戻すときは選択も一緒にクリアする（`toggleSelectionMode`）。 */
	let selectionMode = $state(false);

	// --- edit ---
	let selected: Tag | null = $state(null);
	let editForm = $state(blankForm());
	/** T18-1（TAG-UX-C 一部）: `createBaseline` と同じ役割、edit 版。 */
	let editBaseline: FormState = blankForm();
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);

	/**
	 * T18-2a（TAG-UX-B「詳細を閉じても値保持・詳細エラー時は自動展開」）:
	 * `tagFields` の3つの `<details class="detail-group">` の開閉状態。
	 * create/edit の Drawer は `{#if drawerMode === 'create'} ...
	 * {:else if drawerMode === 'edit'}` で同時に1つしかマウントされないが、
	 * それぞれ独立した `$state` を持たせる — 共有すると、例えば edit で
	 * 「しきい値」セクションを開いた状態のまま create Drawer を開いたときに
	 * その開閉状態が漏れてしまう。
	 */
	interface DetailOpenState {
		display: boolean;
		threshold: boolean;
		write: boolean;
	}
	function blankDetailOpen(): DetailOpenState {
		return { display: false, threshold: false, write: false };
	}
	let createDetailOpen: DetailOpenState = $state(blankDetailOpen());
	let editDetailOpen: DetailOpenState = $state(blankDetailOpen());

	/**
	 * T18-2b（docs/banto-hub-t18-design.md「T18-2b プロトコル別アドレス補助」、
	 * TAG-UX-6「入力中に共通 preflight を実行する」）: アドレス入力中の
	 * デバウンス dry-run 検証状態。新規バックエンドは作らず、連続登録/CSV
	 * インポートが使っているのと同じ `createTagsBatch(tags, dryRun=true)`
	 * （`POST /api/tags/batch`）を単票1件で呼ぶ。`checkedFor` は最後に検証を
	 * 実行した時点の `toInput(form)` の JSON（`continuousTagsJson`/
	 * `csvTagsJson` の「鮮度」判定と同じ発想 - ただしここでは登録可否の
	 * ゲートには使わず、あくまで参考表示なので `$derived` の fresh 判定までは
	 * 持たない）。`result` が `null` のままなのは「まだ検証していない/
	 * 検証条件を満たさない」と「ネットワークエラーで検証できなかった」の
	 * 両方 - 入力中の補助表示であり、実際の正しさの最終防衛は既存どおり
	 * submit 時の preflight（`applyFieldErrors`）なので、ここで通信エラーを
	 * トーストで騒がしく出したりはしない。
	 */
	interface AddressPreflightState {
		checking: boolean;
		result: BatchTagsResult | null;
	}
	function blankAddressPreflight(): AddressPreflightState {
		return { checking: false, result: null };
	}
	let createAddressPreflight: AddressPreflightState = $state(blankAddressPreflight());
	let editAddressPreflight: AddressPreflightState = $state(blankAddressPreflight());
	let addressPreflightTimer: ReturnType<typeof setTimeout> | undefined;

	/**
	 * アドレス欄の `oninput` から呼ぶ。名前・収集グループ・アドレスの
	 * いずれかが未入力ならまだ有効なプレビュー対象を作れないので、直前の
	 * 結果を消して何もしない（グループ未選択で protocol が定まらない状態の
	 * dry-run は「configuration」エラーが名前欄の空欄理由で埋まるだけの
	 * ノイズになるため）。400ms のデバウンスはタイプ中の連打で毎打鍵
	 * `/api/tags/batch` を叩かないようにするための単純なタイマー -
	 * 連続登録の「検証」ボタン（明示クリック）と違い、これは自動発火なので
	 * 控えめな間隔にしている。
	 */
	function scheduleAddressPreflight(form: FormState, target: 'create' | 'edit'): void {
		if (addressPreflightTimer !== undefined) clearTimeout(addressPreflightTimer);
		const ready =
			form.tagKind === 'plc' &&
			form.name.trim() !== '' &&
			form.collectionGroupId !== '' &&
			form.address.trim() !== '';
		if (!ready) {
			if (target === 'create') createAddressPreflight = blankAddressPreflight();
			else editAddressPreflight = blankAddressPreflight();
			return;
		}
		addressPreflightTimer = setTimeout(() => void runAddressPreflight(form, target), 400);
	}

	async function runAddressPreflight(form: FormState, target: 'create' | 'edit'): Promise<void> {
		if (target === 'create') createAddressPreflight = { ...createAddressPreflight, checking: true };
		else editAddressPreflight = { ...editAddressPreflight, checking: true };
		let result: BatchTagsResult | null;
		try {
			result = await createTagsBatch([toInput(form)], true);
		} catch {
			// 通信エラー等はここでは無視する - このコメント直上の型doc参照。
			result = null;
		}
		const next: AddressPreflightState = { checking: false, result };
		if (target === 'create') createAddressPreflight = next;
		else editAddressPreflight = next;
	}

	/**
	 * これらの `$effect` は該当セクションにエラーが**新たに現れたときだけ
	 * 開く**方向にしか作用しない（強制的に閉じることはしない）— ユーザーが
	 * 手動でセクションを折りたたんだ選択は、次に失敗した送信が
	 * `createErrors`/`editErrors` を丸ごと再代入して effect を再実行させる
	 * まで尊重される（`handleCreate`/`saveEdit` は毎回 `xxxErrors = {}` →
	 * 失敗時に新しいオブジェクトを代入し直すので、この effect はキー入力の
	 * たびにではなく実際の送信試行のたびにしか再実行されない）。
	 */
	$effect(() => {
		if (hasFieldError(createErrors, DISPLAY_SCALING_FIELDS)) createDetailOpen.display = true;
		if (hasFieldError(createErrors, THRESHOLD_FIELDS)) createDetailOpen.threshold = true;
		if (hasFieldError(createErrors, WRITE_SAFETY_FIELDS)) createDetailOpen.write = true;
	});
	$effect(() => {
		if (hasFieldError(editErrors, DISPLAY_SCALING_FIELDS)) editDetailOpen.display = true;
		if (hasFieldError(editErrors, THRESHOLD_FIELDS)) editDetailOpen.threshold = true;
		if (hasFieldError(editErrors, WRITE_SAFETY_FIELDS)) editDetailOpen.write = true;
	});

	/**
	 * T18-1（TAG-UX-C 4点目「差分表示 UI」、docs/banto-hub-desktop-plan.md
	 * §9.4）: revision 競合発生時のフィールド単位差分。`local` は競合検出
	 * 時点の編集フォームのスナップショット（以後の入力とは連動しない）、
	 * `serverForm`/`serverTag` はその時点のサーバー最新値。ユーザーが
	 * 「サーバー最新を採用」/「自分の内容で再保存」のいずれかを選ぶまで
	 * 保持し、パネル表示に使う。
	 */
	type EditConflict = {
		local: FormState;
		serverForm: FormState;
		serverTag: Tag;
		fields: ConflictFieldDiff[];
	};
	let editConflict: EditConflict | null = $state(null);

	function selectTag(t: Tag): void {
		if (!confirmDiscardIfNeeded()) return;
		selected = t;
		editForm = formFromTag(t);
		editBaseline = formFromTag(t);
		editErrors = {};
		editConflict = null;
		editAddressPreflight = blankAddressPreflight();
		drawerMode = 'edit'; // T13-1: 行クリック編集はドロワーで開く
	}

	/**
	 * T18-3b: 選択モードのON/OFF切り替え。OFFへ戻すときは選択集合もクリア
	 * する - 選択モードを抜けたのに一括操作バーだけ残る（選択済みなのに
	 * 何を選んだか画面上でもう分からない）状態を避けるため。
	 *
	 * T18-3e: 表編集モード（`gridEditMode`）とは相互排他 - ONにする前に
	 * 表編集モードが立っていれば、保留中のセル編集を破棄した上で（キャンセル
	 * されたら選択モードへは切り替えない）まず表編集モードを終了する
	 * （実装指示「selectionMode と相互排他」）。
	 */
	function toggleSelectionMode(): void {
		if (!selectionMode && gridEditMode) {
			if (!confirmDiscardPendingCellEdits()) return;
			gridEditMode = false;
		}
		selectionMode = !selectionMode;
		if (!selectionMode) selectedIds = new Set();
	}

	/** T18-3b: 選択モード中の行クリック — 選択のON/OFFを切り替える（`selectTag` の代わりに `onRowClick` へ渡す）。 */
	function toggleSelectRow(t: Tag): void {
		const next = new Set(selectedIds);
		if (next.has(t.id)) next.delete(t.id);
		else next.add(t.id);
		selectedIds = next;
	}

	/** T18-3b: 選択中の行を BantoGrid の `rowClass` 経由で強調表示する（M14/T9-2 と同じ仕組み、下の CSS 参照）。 */
	function tagRowClass(t: Tag): string | undefined {
		return selectedIds.has(t.id) ? 'tag-row-selected' : undefined;
	}

	// --- T18-3e: BantoGrid セル編集/TSV貼付 (docs/banto-hub-t18-design.md
	// 「T18-3e BantoGrid セル編集/TSV貼付の接続」) ---------------------------
	//
	// 「表編集モード」トグル - selectionMode と同型だが、こちらは ON の間
	// `columns`（下の `$derived.by`）へ `enabled`/`writable`/`unit`/`decimals`
	// の `editable` を付与する。BantoGrid は `editable` を持つ列が1つでも
	// あると単一クリックの `onRowClick` を発火させなくなる仕様
	// （`node_modules/@banto/grid-svelte` の `hasEditableColumns`）ため、OFF
	// の間は `columns` に `editable` キー自体を含めず、既存の単一クリック
	// 編集・複数選択（selectionMode）を厳密に維持する。

	/** T18-3e: 表編集モードのON/OFF。既定 OFF（既存挙動を完全維持）。 */
	let gridEditMode = $state(false);

	/**
	 * 保留中のセル編集（即保存しない - `onCellEdit`/`onRangePaste` はここに
	 * 積むだけ）。「保存」操作時に `buildTagCellEditBatch` へ渡して
	 * `BatchTagUpdateRow[]` を組み立てる。
	 */
	let pendingCellEdits = $state<TagCellEditInput[]>([]);

	/**
	 * 保留編集を id ごとにマージした上書き値 - グリッド表示のローカル上書きに
	 * 使う（`gridDisplayRows` - `filteredTags`/`tags` より後（下の「T13-1:
	 * ツリーフィルタ + 検索」節）で定義している。`filteredTags` に依存する
	 * `$derived` は、TS の TDZ 解析（svelte-check）に引っかからないよう
	 * `filteredTags` 宣言より後ろに置く必要があるため）。
	 */
	const pendingCellOverridesById = $derived(mergeTagCellEdits(pendingCellEdits));

	/** `buildTagCellEditBatch` の結果 - 保存確認パネル・保留バーの件数表示・適用行に使う。 */
	const cellEditBatch = $derived(buildTagCellEditBatch(pendingCellEdits, tags));
	const cellEditRowsJson = $derived(JSON.stringify(cellEditBatch.rows));

	/** 連続登録/CSV/一括操作と同じ「検証済みかどうか」の鮮度追跡（`csvUpdateValidatedFresh` 等と同型）。 */
	let cellEditValidatedRowsJson = $state<string | null>(null);
	let cellEditValidationResult = $state<BatchTagsUpdateResult | null>(null);
	let cellEditValidating = $state(false);
	let cellEditApplying = $state(false);
	/** 保存確認パネル（差分＋preflight結果）の開閉。 */
	let cellEditPanelOpen = $state(false);

	const cellEditValidatedFresh = $derived(
		cellEditBatch.rows.length > 0 &&
			cellEditRowsJson === cellEditValidatedRowsJson &&
			cellEditValidationResult?.ok === true
	);

	/** 保留中のセル編集を全て破棄する（「破棄」ボタン、表編集モードOFF、保存成功後の後始末で共通に使う）。 */
	function discardGridEdits(): void {
		pendingCellEdits = [];
		cellEditValidationResult = null;
		cellEditValidatedRowsJson = null;
		cellEditPanelOpen = false;
	}

	/** 保留編集が無ければ確認不要で `true`。あれば `window.confirm` で破棄確認する（`confirmDiscardIfNeeded` と同じ流儀）。 */
	function confirmDiscardPendingCellEdits(): boolean {
		if (pendingCellEdits.length === 0) return true;
		if (!window.confirm('保留中のセル編集を破棄します。よろしいですか？')) return false;
		discardGridEdits();
		return true;
	}

	/**
	 * 表編集モードのトグル。ONにするには収集停止中である必要がある
	 * （実装指示「停止中ロック」、ボタン自体も `!collectionStopped` で
	 * disabled にする - これは二重ガード）。ONにする前に選択モードが
	 * 立っていれば終了する（相互排他、`toggleSelectionMode` と対称）。
	 * OFFにするときは保留編集があれば破棄確認する。
	 */
	function toggleGridEditMode(): void {
		if (!gridEditMode) {
			if (!collectionStopped) return;
			if (selectionMode) {
				selectionMode = false;
				selectedIds = new Set();
			}
			gridEditMode = true;
			return;
		}
		if (!confirmDiscardPendingCellEdits()) return;
		gridEditMode = false;
	}

	/**
	 * BantoGrid `onCellEdit`（1セル編集）。BantoGrid の列 `validate` が既に
	 * 不正値を弾いた後にしか呼ばれないため、ここでは保留バッファへ積むだけ
	 * （即保存しない）。`editable` は `gridEditMode && collectionStopped` の
	 * ときしか true にならないので、このハンドラ自体は常時登録したままで
	 * 安全（`editable` が false のセルはそもそも edit セッションへ入れない）。
	 */
	async function handleGridCellEdit(edit: CellEdit<Tag>): Promise<void> {
		pendingCellEdits = [
			...pendingCellEdits,
			{ id: Number(edit.rowId), field: edit.field as EditableTagField, value: edit.value }
		];
	}

	/**
	 * BantoGrid `onRangePaste`（TSV貼付、Excel からの貼り付け）。
	 * `edits` は BantoGrid が既に「editable な列のみ・既存行のみ（行は増え
	 * ない）・`validate` 通過済み」に絞り込んだ後の配列 - ここでも保留
	 * バッファへ積むだけでよい。`skipped`（編集不可列・validate 失敗・
	 * アドレス範囲外等でスキップされたセル数）は簡易トーストで知らせる。
	 */
	async function handleGridRangePaste(
		edits: CellEdit<Tag>[],
		info: { skipped: number }
	): Promise<void> {
		if (edits.length > 0) {
			pendingCellEdits = [
				...pendingCellEdits,
				...edits.map((e) => ({
					id: Number(e.rowId),
					field: e.field as EditableTagField,
					value: e.value
				}))
			];
		}
		if (info.skipped > 0) {
			toastStore.push(
				'error',
				`${info.skipped} 件のセルは貼り付けできませんでした（編集不可の列・不正な値など）`
			);
		}
	}

	/**
	 * 「保存」— `buildTagCellEditBatch` で組み立てた行を `updateTagsBatch`
	 * の dry-run（全構成 preflight）にかけ、結果を確認パネルに表示する
	 * （実装指示「保留バッファ→preflight→差分確認→all-or-nothing 適用」）。
	 * 保存直前に収集状態を再確認する - 稼働中に切り替わっていれば preflight
	 * すら投げず、確認パネルも開かない（停止中ロックの最終防波堤）。
	 */
	async function handleSaveGridEdits(): Promise<void> {
		if (cellEditBatch.rows.length === 0) return;
		await loadHubStatus();
		if (!collectionStopped) {
			toastStore.push(
				'error',
				'収集稼働中は表編集を保存できません。収集を停止してから再度お試しください。'
			);
			return;
		}
		cellEditValidating = true;
		cellEditValidationResult = null;
		try {
			const result = await updateTagsBatch(cellEditBatch.rows, true);
			cellEditValidationResult = result;
			cellEditValidatedRowsJson = cellEditRowsJson;
			cellEditPanelOpen = true;
			if (!result.ok) {
				toastStore.push('error', 'エラーがあります。下の一覧を確認してください。');
			}
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			cellEditValidating = false;
		}
	}

	/** 確認パネルの「閉じる」- 保留バッファは維持したまま、パネルだけ閉じる（続けて編集・再保存できる）。 */
	function cancelCellEditConfirm(): void {
		cellEditPanelOpen = false;
	}

	/**
	 * 確認パネルの「この内容で保存を適用」- `dryRun: false` の本適用
	 * （all-or-nothing）。202（稼働中キュー投入）を含む例外は他の書き込み系
	 * 呼び出しと同じ汎用エラートーストに委ねる（停止中ロックがあるため通常
	 * 到達しないが、最終防波堤として同じ扱いにしておく）。
	 */
	async function handleApplyGridEdits(): Promise<void> {
		if (!cellEditValidatedFresh) return;
		cellEditApplying = true;
		try {
			const result = await updateTagsBatch(cellEditBatch.rows, false);
			cellEditValidationResult = result;
			if (result.ok) {
				toastStore.push('success', `${result.count}件のセル編集を保存しました`);
				monitorCtaHref = monitorHref({
					groupId: soleGroupId(cellEditBatch.rows.map((r) => r.collectionGroupId))
				});
				discardGridEdits();
				await reload();
			} else {
				toastStore.push('error', '一部のセルでエラーが発生しました。下の一覧を確認してください。');
			}
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			cellEditApplying = false;
		}
	}

	/** 保存確認パネルの「変更内容」列 - `formatCsvUpdateDiffs` と同じ「field: from → to」を複数フィールド分カンマ区切りにする表示。 */
	function formatCellEditDiffs(diffs: ConflictFieldDiff[]): string {
		return diffs.map((d) => `${d.label}: ${d.local} → ${d.server}`).join(', ');
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updateTag(selected.id, {
				...toInput(editForm),
				expectedRevision: selected.revision
			});
			toastStore.push('success', '更新しました');
			monitorCtaHref = monitorHref({
				groupId: updated.collectionGroupId,
				focus: [externalNameForTag(updated)]
			});
			selected = updated;
			// 保存成功後はサーバーの正規化値を基準に取り直す - 未保存
			// 変更は無くなったので直後の dirty 判定は false になる。
			editForm = formFromTag(updated);
			editBaseline = formFromTag(updated);
			editAddressPreflight = blankAddressPreflight();
			// T18-1（TAG-UX-C 4点目、差分表示 UI）: 競合パネル表示中に
			// フォームを直接編集して再送信（コンフリクトの解決ボタンを経由
			// しない経路）しても保存が成功したら差分パネルは消す - パネルが
			// 参照する `editConflict.local` はもう最新の保存内容ではない。
			editConflict = null;
			await reload();
		} catch (err) {
			// T18-1（docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目、
			// 差分表示 UI）: 他クライアントが先にこのタグを更新済み
			// （revision 不一致）。「黙って上書きしない」の受け入れ基準どおり
			// 成功トーストは出さないが、ローカルの未保存編集はもう破棄しない -
			// `editForm` はユーザーが送ろうとした値のまま残し、
			// `editBaseline` をサーバー最新値に差し替えて dirty のままにする
			// （破棄確認の対象として残す）。フィールド単位の差分は
			// `diffFormRecords` で求めて `editConflict` に保持し、パネル表示
			// に使う。
			if (isTagRevisionConflictError(err)) {
				const local = { ...editForm };
				const serverForm = formFromTag(err.current);
				const fields = diffFormRecords(
					conflictRecord(local),
					conflictRecord(serverForm),
					FIELD_LABELS
				);
				selected = err.current;
				tags = tags.map((t) => (t.id === err.current.id ? err.current : t));
				editBaseline = serverForm;
				editErrors = {};
				editConflict = { local, serverForm, serverTag: err.current, fields };
				toastStore.push('error', err.message);
				return;
			}
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) editErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	/**
	 * T18-1（TAG-UX-C 4点目、差分表示 UI）: 競合パネルの「サーバー最新を
	 * 採用」— ローカルの未保存編集を捨て、フォームをサーバー最新値に
	 * 揃える（`selected` は競合検出時に既にサーバー最新へ更新済み）。
	 */
	function resolveConflictWithServer(): void {
		if (!editConflict) return;
		editForm = editConflict.serverForm;
		editBaseline = editConflict.serverForm;
		editConflict = null;
	}

	/**
	 * T18-1（TAG-UX-C 4点目、差分表示 UI）: 競合パネルの「自分の内容で
	 * 再保存」— `editForm` はローカルの編集内容のまま変更せず、`selected`
	 * も既にサーバー最新の revision を指しているため、`saveEdit()` を
	 * そのまま呼び直せば `expectedRevision` が最新値になり保存が通る。
	 */
	async function resolveConflictWithLocal(): Promise<void> {
		editConflict = null;
		await saveEdit();
	}

	/**
	 * T18-1（TAG-UX-C 続き）: 削除中も `deleting` を立てて `isDrawerBusy()`
	 * に含める - creating/saving と同じ try/finally パターン。これが無いと
	 * 削除ボタンを連打できたり、削除中に保存・×で閉じる等の他操作が
	 * 走ってしまう（相互排他が破れる）。
	 */
	let deleting = $state(false);

	/**
	 * T18-1（TAG-UX-C 5点目、docs/banto-hub-desktop-plan.md §9.4「削除前に
	 * 演算タグ等の参照影響と完全な外部名を表示する」）: `tag` の完全外部名
	 * （`{接続}.{グループ}.{タグ}`）を組み立てる。`groupName`/`connectionName`
	 * は表示用のフォールバック（`#${id}`/`undefined`）を持つが、外部名は
	 * 未解決でも `?` で埋めて必ず3セグメントの形にする（通常は起こらない -
	 * `tags`/`groups`/`connections` は同じ `reload()` で一括取得している）。
	 */
	function externalNameForTag(tag: Tag): string {
		const group = groups.find((g) => g.id === tag.collectionGroupId);
		const connName = group ? connectionName(group.plcConnectionId) : undefined;
		return buildExternalName(connName ?? '?', group?.name ?? `#${tag.collectionGroupId}`, tag.name);
	}

	/**
	 * T18-4c: 複数タグの `collectionGroupId` が単一グループに収まっている
	 * かを判定する。単一ならその ID を返し、空・複数グループへ跨る場合は
	 * `null`（`monitorHref({ groupId: null, ... })` は `group` パラメータを
	 * 付けない = 素の `/monitor` への絞り込み無しリンクになる - CSV/一括
	 * 操作のように対象が複数グループへ跨りうる経路の CTA で使う）。
	 */
	function soleGroupId(ids: number[]): number | null {
		if (ids.length === 0) return null;
		const first = ids[0];
		return ids.every((id) => id === first) ? first : null;
	}

	async function handleDelete(): Promise<void> {
		if (!selected) return;
		const externalName = externalNameForTag(selected);
		// 削除対象を参照している演算タグを一覧して確認文言に含める - サーバー側の
		// 削除 preflight（参照切れで失敗）はそのまま正しさの最終バックストップで
		// あり、クライアント側でハードブロックはしない（確認 OK なら従来どおり
		// deleteTag を呼ぶ - サーバーが拒否した場合は既存の error toast で通知）。
		const referencing = findReferencingComputedTags(
			selected.id,
			externalName,
			tags,
			groups,
			connections
		);
		if (!window.confirm(formatDeleteConfirmMessage(externalName, referencing))) return;
		deleting = true;
		try {
			await deleteTag(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			drawerMode = null; // 削除後は編集対象が無いのでドロワーを閉じる
			editConflict = null;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			deleting = false;
		}
	}

	// --- T13-1: Drawer の表示制御 (docs/ux-plan.md §4b) ---------------------
	//
	// `selected`（編集対象、上で宣言済み）と `createForm`/`createErrors`
	// （このあと「連続登録」セクションの前の「create」ブロックで宣言 -
	// 実際には上の「edit」ブロックより前に既に宣言済み）を参照するため、
	// 両方が揃うこの位置に置く。

	const drawerTitle = $derived.by((): string => {
		switch (drawerMode) {
			case 'create':
				return '新規作成';
			case 'edit':
				return selected ? `${selected.name} を編集` : '編集';
			case 'continuous':
				return '連続登録';
			case 'csv':
				return 'CSVインポート';
			default:
				return '';
		}
	});

	// 連続登録・CSVインポートはプレビュー/エラー一覧のテーブルが横に
	// 広いため、通常登録・編集より少し広いドロワー幅を使う。
	const drawerWidth = $derived(
		drawerMode === 'continuous' || drawerMode === 'csv' ? '640px' : '480px'
	);

	/**
	 * T18-1（TAG-UX-C 一部、docs/banto-hub-desktop-plan.md §9.4）: 現在開いて
	 * いる Drawer が busy（作成/保存/削除/検証/登録のいずれかを実行中）
	 * かどうか。`drawerMode` は常に高々1つしか開いていないため、これらの
	 * フラグのどれか1つでも立っていれば「今開いている Drawer」の処理中と
	 * みなせる。削除も busy に含める（TAG-UX-C 続き、
	 * `cursor/t18-1-drawer-busy-e3cb`）- 削除中に保存や再削除、×での
	 * クローズができてしまうのを防ぐ。
	 */
	function isDrawerBusy(): boolean {
		return (
			creating ||
			saving ||
			deleting ||
			validating ||
			applyingContinuous ||
			csvValidating ||
			csvApplying ||
			csvUpdateValidating ||
			csvUpdateApplying
		);
	}

	/**
	 * T18-1（TAG-UX-C 一部）: 現在開いている Drawer のフォームが、開いた
	 * 時点のスナップショット（`*Baseline`）から変更されているか。
	 * CSV インポートはテキスト入力のフォームを持たないため、
	 * 「ファイルを解析済みか（`csvParseResult !== null`）」を dirty 相当
	 * として扱う（同じ `isFormDirty` ヘルパーで一貫させる - baseline 側は
	 * 常に未解析状態の `null`）。
	 */
	function isDrawerDirty(): boolean {
		switch (drawerMode) {
			case 'create':
				return isFormDirty(createBaseline, createForm);
			case 'edit':
				return isFormDirty(editBaseline, editForm);
			case 'continuous':
				return isFormDirty(continuousBaseline, continuousForm);
			case 'csv':
				return isFormDirty(null, csvParseResult);
			default:
				return false;
		}
	}

	/**
	 * T18-1（TAG-UX-C 一部）: Drawer を閉じる・別のタグ行を選ぶ・画面遷移
	 * する、のいずれの経路でも同じ確認を行う共通ヘルパー。
	 * - busy 中は何もできない（`false` を返し、呼び出し元の操作を止める）。
	 * - dirty なら `window.confirm` で確認し、キャンセルされたら `false`。
	 * - busy でも dirty でもなければ `true`（即座に進めてよい）。
	 */
	function confirmDiscardIfNeeded(): boolean {
		if (isDrawerBusy()) return false;
		if (drawerMode !== null && isDrawerDirty() && !window.confirm('変更を破棄しますか？')) {
			return false;
		}
		return true;
	}

	/**
	 * T18-2d（docs/banto-hub-t18-design.md「T18-2d 初回導線チェックリスト」、
	 * TAG-UX-A「ツリーで選択中の接続／グループを単票・連続登録へプリセット
	 * する」）: 開いた時点でツリーがグループを選択中なら、そのグループを
	 * `collectionGroupId` へ初期値として入れる（`resolveGroupIdFromTreeSelection`
	 * が「すべて」/接続選択/calc・mem配下は `null` にする - 冒頭 import 元の
	 * doc comment 参照）。プリセットした値も `createBaseline` に含めるので
	 * dirty 扱いにはならない（TAG-UX-A「選択グループからタグを作る場合、
	 * グループの再選択を要求しない」を満たしつつ、まだ何も入力していない
	 * 状態を dirty と誤認しない）。
	 */
	function openCreateDrawer(): void {
		if (!confirmDiscardIfNeeded()) return;
		const presetGroupId = resolveGroupIdFromTreeSelection(treeFilter, groups, connections);
		const next = blankForm();
		if (presetGroupId !== null) next.collectionGroupId = String(presetGroupId);
		createForm = next;
		createBaseline = { ...next };
		createErrors = {};
		createAddressPreflight = blankAddressPreflight();
		editConflict = null;
		duplicateSource = null; // T18-3a: 通常の新規作成では複製元差分パネルを出さない
		// 2026-09-01: 空フォームなので「名前が空」から始まる - プリフィル対象。
		createNameTouched = false;
		// T19 S1-b（UX-34）: 新規タグは常に `writable` の自動計算から始まる
		// （上の `createWritableTouched` 宣言のコメント参照）。
		createWritableTouched = false;
		drawerMode = 'create';
	}

	/**
	 * T18-3a（docs/banto-hub-t18-design.md「T18-3a タグ複製」、TAG-UX-D
	 * 前半「『このタグを複製』、型/単位/スケーリング/しきい値を引継ぎ名前と
	 * アドレスのみ変更する」）: `t` を複製元に create Drawer を開く。
	 * `openCreateDrawer` を土台にし、`blankForm()` の代わりに「複製元タグ→
	 * フォーム変換」（edit フローが使っている `formFromTag`、同じ変換を
	 * ここでも再利用する）→ `buildDuplicateFormValues`（`tagDuplicate.ts`）
	 * で名前・アドレスだけ調整した値を `createForm`/`createBaseline` の
	 * 初期値にする。`createBaseline` にも同じ値を入れるのは
	 * `openCreateDrawer` の既存 preset と同じ理由 - まだ何も入力していない
	 * 状態（複製名・空アドレスが入っただけ）を dirty と誤認しないため。
	 *
	 * `drawerMode` は `'create'` のまま（複製も「新規作成」であり、保存は
	 * 既存 `handleCreate`/`createTag` をそのまま使う - 既存タグを上書き
	 * しない受け入れ条件は、この経路が常に POST 新規作成であることで自然に
	 * 満たされる）。`duplicateSource` に複製元タグを保持し、`duplicateDiff`
	 * （上で宣言済みの `$derived`）が保存前の差分パネルに使う。
	 */
	function openDuplicateDrawer(t: Tag): void {
		if (!confirmDiscardIfNeeded()) return;
		// 2026-08-31 オーナー決定: タグ名の一意性は全体一意→収集グループ内一意へ
		// 緩和された（サーバー側 `crates/banto-tags` migration 0011）。複製名が
		// 避けるべき既存名も複製元と同じ収集グループ内のものだけでよい -
		// 他グループの同名タグは合法な同名で、それを理由に `_copy2` へ
		// 繰り上げるのは不要な事故防止（最終的な一意性検証は既存どおり
		// サーバー側 `createTag` が正 - `tagDuplicate.ts` の doc comment参照）。
		const existingNames = tags
			.filter((tag) => tag.collectionGroupId === t.collectionGroupId)
			.map((tag) => tag.name);
		const next = buildDuplicateFormValues(formFromTag(t), existingNames);
		createForm = next;
		createBaseline = { ...next };
		createErrors = {};
		createAddressPreflight = blankAddressPreflight();
		editConflict = null;
		duplicateSource = t;
		// 2026-09-01: 複製名（`{元名}_copy` 等）は既に意味のある値が入って
		// いるため、プリフィル対象外として開始する（上の `createNameTouched`
		// 宣言のコメント参照 - アドレスを後から入力しても複製名を上書きしない）。
		createNameTouched = true;
		// T19 S1-b（UX-34）: 複製元の `writable` は「型/単位/スケーリング/
		// しきい値」と同格の引き継ぎ対象 - 自動計算で上書きしない（上の
		// `createWritableTouched` 宣言のコメント参照）。
		createWritableTouched = true;
		drawerMode = 'create';
	}

	function openContinuousDrawer(): void {
		if (!confirmDiscardIfNeeded()) return;
		continuousBaseline = blankContinuousForm();
		editConflict = null;
		drawerMode = 'continuous';
	}

	function openCsvDrawer(): void {
		if (!confirmDiscardIfNeeded()) return;
		editConflict = null;
		drawerMode = 'csv';
	}

	function closeDrawer(): void {
		drawerMode = null;
		// T18-1（TAG-UX-C 4点目、差分表示 UI）: Drawer を閉じたら競合パネルの
		// 状態も破棄する（`confirmDiscardIfNeeded` 経由の破棄確認は
		// `onRequestClose` が既に済ませている — ここは後始末のみ）。
		editConflict = null;
		// T18-3a: 複製元差分パネルの状態も、Drawer を閉じたらクリアする。
		duplicateSource = null;
		// T18-2b: 保留中のデバウンス preflight があれば止める - 閉じた後の
		// Drawer に対して古い結果が届いても表示先が無いので無害だが、
		// 不要な `/api/tags/batch` 呼び出し自体を止めておく。
		if (addressPreflightTimer !== undefined) clearTimeout(addressPreflightTimer);
	}

	// T18-1: 画面遷移（サイドバーの他画面リンク等）でも Esc/× と同じ破棄
	// 確認を行う。`confirmDiscardIfNeeded` が `false` を返した（busy 中、
	// または dirty で確認をキャンセルされた）場合は遷移そのものを止める。
	// T18-3e: 保留中のセル編集がある場合も同様に確認する（Drawer は開いて
	// いなくても、表編集の保留バッファは未保存の変更のため）。
	beforeNavigate((nav) => {
		if (drawerMode !== null && !confirmDiscardIfNeeded()) {
			nav.cancel();
			return;
		}
		if (pendingCellEdits.length > 0 && !confirmDiscardPendingCellEdits()) {
			nav.cancel();
		}
	});

	// --- T13-1: ツリーフィルタ + 検索 (docs/ux-plan.md §4b) -----------------
	//
	// ツリー選択（接続 or グループ）と検索ボックス（名前・アドレスの部分
	// 一致、クライアントサイド）を両方満たす行だけを右ペインの BantoGrid
	// に渡す。サーバーへの問い合わせは発生しない（`tags` は既に全件
	// ロード済み）。

	type TreeFilter =
		{ type: 'all' } | { type: 'connection'; id: number } | { type: 'group'; id: number };
	let treeFilter: TreeFilter = $state({ type: 'all' });
	let searchQuery = $state('');

	const treeSelectedId = $derived.by((): string => {
		if (treeFilter.type === 'all') return 'all';
		if (treeFilter.type === 'connection') return `conn:${treeFilter.id}`;
		return `group:${treeFilter.id}`;
	});

	function handleTreeSelect(data: ConnectionTreeNodeData): void {
		if (data.kind === 'all') treeFilter = { type: 'all' };
		else if (data.kind === 'connection')
			treeFilter = { type: 'connection', id: data.connection.id };
		else treeFilter = { type: 'group', id: data.group.id };
	}

	/**
	 * T18-2e（docs/banto-hub-t18-design.md「T18-2e T13-3 移管」、
	 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A）: ツリーの右クリック
	 * （`ConnectionTree`/`TreeView` の `oncontextmenu` - マウス右クリックと
	 * `Shift+F10`/メニューキーの両方がここに集約される）から出す、階層に
	 * 応じたメニュー。常時表示の「新規登録」等の主操作
	 * （`openCreateDrawer`/上のツールバー）はそのまま残す - このメニューは
	 * それを置き換えない。
	 *
	 * T18-6d（TAG-UX-7、2026-08-27 オーナー決定）追記: T18-2e 時点は「作成」
	 * 1項目だけだったが、ここに「接続/グループの再設定・削除」を追加した
	 * （既存のタグ作成項目は壊さず同じメニューに項目を足す - 実装指示の
	 * 制約）。項目の種別・ラベル・対象 ID の決定自体は依存ゼロの純関数
	 * `resolveTreeContextMenuItems`（`tagTreeContextMenu.ts`）に委ね、ここは
	 * DOM 状態（メニューの表示位置・選択ノードの反映）と実行（Drawer 起動）
	 * だけを担う。
	 *
	 * T19 S1-a（docs/banto-hub-t19-design.md §7.1「viewer ロールからの接続・
	 * グループ詳細の閲覧」）追記: 以前はこのハンドラ自体を `canWrite` の
	 * ときだけ `oncontextmenu` に配線していた（viewer は右クリックしても
	 * 何も起きなかった）。旧 `plc-connections`/`collection-groups` 画面の
	 * グリッドは全ロールが閲覧できていたのに対し、ツリー一本化後は viewer が
	 * `host`/`port` 等を見る手段が無くなる（設計 §7.1「必須」項目）ため、
	 * `oncontextmenu` は常時配線し、`canWrite` に応じて書き込み用メニュー
	 * （作成・再設定・削除）と viewer 向けメニュー（「詳細を表示」の1項目
	 * のみ）を切り替える判断自体は `resolveTreeContextMenuItemsForRole`
	 * （`tagTreeContextMenu.ts`、依存ゼロの純関数）へ切り出し済み - ここでは
	 * `canWrite` を渡すだけにして、分岐そのものを単体テストで固定できる
	 * ようにする（コードレビュー指摘、2026-09-02: E2E は試運転モードでは
	 * role 差を検証できないため、この分岐は単体テストが最終防衛線になる）。
	 * 書き込み権限がある利用者の挙動（メニュー内容・実行結果）は一切
	 * 変えていない。
	 */
	interface TreeContextMenuState {
		x: number;
		y: number;
		items: TreeContextMenuItemAction[];
		/** メニューを閉じたときにフォーカスを戻す元要素（開いた時点の `document.activeElement`）。 */
		triggerEl: HTMLElement | null;
	}
	let treeContextMenu: TreeContextMenuState | null = $state(null);

	/**
	 * 右クリックされたノードを選択状態にする - 「親（接続/グループ）は
	 * 選択ノードからプリセットする」（実装指示 T18-2e スコープ1点目）を、
	 * 既存の `resolveGroupIdFromTreeSelection`（T18-2d、`openCreateDrawer` が
	 * 使う）にそのまま乗せるため。T19 S1-a 以降、書き込み権限がある利用者は
	 * `calc`/`mem` 配下でも常にメニューが出る（`resolveTreeContextMenuItems`
	 * が空配列を返すことは無くなった - 上の doc comment 参照）。メニューが
	 * 空になるのは viewer が「すべて」ノードを右クリックした場合のみ
	 * （`resolveReadOnlyTreeContextMenuItems` が閲覧対象無しとして `[]` を
	 * 返す）- その場合も選択だけは反映してメニューは出さない。
	 */
	function handleTreeContextMenu(
		node: TreeNode<ConnectionTreeNodeData>,
		position: { x: number; y: number }
	): void {
		handleTreeSelect(node.data);
		const items = resolveTreeContextMenuItemsForRole(node.data, canWrite);
		if (items.length === 0) {
			treeContextMenu = null;
			return;
		}
		treeContextMenu = {
			x: position.x,
			y: position.y,
			items,
			triggerEl: document.activeElement instanceof HTMLElement ? document.activeElement : null
		};
	}

	function closeTreeContextMenu(): void {
		const trigger = treeContextMenu?.triggerEl;
		treeContextMenu = null;
		trigger?.focus();
	}

	/**
	 * T18-6d: 接続/収集グループの管理 Drawer 状態。`ConnectionDrawer`/
	 * `CollectionGroupDrawer`（T18-6a/6b、自己完結部品）を、単独ページ
	 * （`/plc-connections`/`/collection-groups`）と全く同じ使い方でこの
	 * ページにも並べて開くだけ - このページ独自の接続/グループ CRUD 処理は
	 * 持たない。保存/削除成功時は既存の `reload()`（groups/connections/tags
	 * を1回で取り直す）をそのまま呼んで、ツリーと右ペインの一覧の両方に
	 * 反映する。
	 */
	let connectionDrawerOpen = $state(false);
	let connectionDrawerTarget: PlcConnection | null = $state(null);
	/** T18-6d: 「接続を削除」から開いた場合だけ `true`（下の doc comment 参照）。 */
	let connectionDrawerRequestDelete = $state(false);
	/**
	 * T19 S1-a（docs/banto-hub-t19-design.md §7.1「viewer ロールからの接続・
	 * グループ詳細の閲覧」）: viewer の「詳細を表示」から開いた場合だけ
	 * `true`。`ConnectionDrawer` を読み取り専用モード（`readOnly` prop）で
	 * 開くためのフラグ - 書き込み系の open系関数（作成/再設定/削除）は
	 * すべて明示的に `false` を設定し、書き込み権限がある利用者の挙動を
	 * 変えない。
	 */
	let connectionDrawerReadOnly = $state(false);

	let groupDrawerOpen = $state(false);
	let groupDrawerTarget: CollectionGroup | null = $state(null);
	let groupDrawerPresetConnectionId: number | null = $state(null);
	/** T18-6d: 「収集グループを削除」から開いた場合だけ `true`（下の doc comment 参照）。 */
	let groupDrawerRequestDelete = $state(false);
	/** T19 S1-a: `connectionDrawerReadOnly` と同じ役割（`CollectionGroupDrawer` 用）。 */
	let groupDrawerReadOnly = $state(false);

	function openConnectionCreateDrawer(): void {
		if (!confirmDiscardIfNeeded()) return;
		closeDrawer(); // タグ Drawer が開いていれば閉じる（同時に複数 Drawer を出さない）。
		connectionDrawerTarget = null;
		connectionDrawerRequestDelete = false;
		connectionDrawerReadOnly = false;
		connectionDrawerOpen = true;
	}

	function openConnectionEditDrawer(connectionId: number): void {
		const target = connections.find((c) => c.id === connectionId);
		if (!target) return; // 通常起きない（右クリック直後は必ず存在する）が、念のため無視する。
		if (!confirmDiscardIfNeeded()) return;
		closeDrawer();
		connectionDrawerTarget = target;
		connectionDrawerRequestDelete = false;
		connectionDrawerReadOnly = false;
		connectionDrawerOpen = true;
	}

	/**
	 * T19 S1-a: viewer の右クリック「詳細を表示」（`resolveReadOnlyTreeContextMenuItems`
	 * の `viewConnection`）から開く。`openConnectionEditDrawer` と対になる
	 * 読み取り専用版 - 対象が見つからない場合の無視、`confirmDiscardIfNeeded`
	 * （viewer は書き込み系フォームを開けないため通常は素通りする）、複数
	 * Drawer を同時に出さないための `closeDrawer()` は同じ考え方を踏襲する。
	 * virtual（calc/mem）接続でも制限しない（閲覧は書き込みと異なり特別扱い
	 * する理由が無い - `resolveReadOnlyTreeContextMenuItems` の doc comment
	 * と同じ理由）。
	 */
	function openConnectionViewDrawer(connectionId: number): void {
		const target = connections.find((c) => c.id === connectionId);
		if (!target) return;
		if (!confirmDiscardIfNeeded()) return;
		closeDrawer();
		connectionDrawerTarget = target;
		connectionDrawerRequestDelete = false;
		connectionDrawerReadOnly = true;
		connectionDrawerOpen = true;
	}

	/**
	 * 「接続を削除」メニュー項目: `ConnectionDrawer` を再設定モードで開き、
	 * `requestDelete` で Drawer 側の既存 `handleDelete` を1回だけ呼ばせる -
	 * 確認ダイアログ・削除影響エラー（収集グループが参照している場合）の
	 * 扱いは `ConnectionDrawer.svelte::handleDelete` の実装をそのまま使い、
	 * ここでは独自の削除処理を持たない（実装指示の制約）。
	 */
	function openConnectionDeleteFlow(connectionId: number): void {
		const target = connections.find((c) => c.id === connectionId);
		if (!target) return;
		if (!confirmDiscardIfNeeded()) return;
		closeDrawer();
		connectionDrawerTarget = target;
		connectionDrawerRequestDelete = true;
		connectionDrawerReadOnly = false;
		connectionDrawerOpen = true;
	}

	function closeConnectionDrawer(): void {
		connectionDrawerOpen = false;
	}

	async function handleConnectionDrawerSaved(): Promise<void> {
		await reload();
	}

	async function handleConnectionDrawerDeleted(): Promise<void> {
		await reload();
	}

	/**
	 * T19 S1-a（docs/banto-hub-t19-design.md §7.1「常時表示の『新規作成』
	 * 入口」）: ツリー上部の常設ボタン「収集グループを追加」から開く場合は
	 * 所属 PLC 接続を未選択のまま出す（旧 `collection-groups` 画面の常設
	 * ボタンと同じ挙動）。ツリーの接続ノード右クリック「収集グループを作成」
	 * （`resolveTreeContextMenuItems` の `createGroup`）は引き続き接続 ID を
	 * 渡してプリセットする - 呼び出し元によって挙動を変えるため、
	 * `presetConnectionId` は省略可能にした（既定 `null` = 未選択）。
	 */
	function openGroupCreateDrawer(presetConnectionId: number | null = null): void {
		if (!confirmDiscardIfNeeded()) return;
		closeDrawer();
		groupDrawerTarget = null;
		groupDrawerPresetConnectionId = presetConnectionId;
		groupDrawerRequestDelete = false;
		groupDrawerReadOnly = false;
		groupDrawerOpen = true;
	}

	function openGroupEditDrawer(groupId: number): void {
		const target = groups.find((g) => g.id === groupId);
		if (!target) return;
		if (!confirmDiscardIfNeeded()) return;
		closeDrawer();
		groupDrawerTarget = target;
		groupDrawerPresetConnectionId = null;
		groupDrawerRequestDelete = false;
		groupDrawerReadOnly = false;
		groupDrawerOpen = true;
	}

	/**
	 * T19 S1-a: viewer の右クリック「詳細を表示」（`viewGroup`）から開く。
	 * `openConnectionViewDrawer` と対になる読み取り専用版 - virtual 接続
	 * （calc/mem）配下のグループでも制限しない。
	 */
	function openGroupViewDrawer(groupId: number): void {
		const target = groups.find((g) => g.id === groupId);
		if (!target) return;
		if (!confirmDiscardIfNeeded()) return;
		closeDrawer();
		groupDrawerTarget = target;
		groupDrawerPresetConnectionId = null;
		groupDrawerRequestDelete = false;
		groupDrawerReadOnly = true;
		groupDrawerOpen = true;
	}

	/**
	 * 「収集グループを削除」メニュー項目: `openConnectionDeleteFlow` と同じ
	 * 考え方 - `CollectionGroupDrawer` を再設定モードで開き、`requestDelete`
	 * で既存の `handleDelete`（タグが参照している場合の Validation エラーを
	 * 含む）を1回だけ呼ばせる。
	 */
	function openGroupDeleteFlow(groupId: number): void {
		const target = groups.find((g) => g.id === groupId);
		if (!target) return;
		if (!confirmDiscardIfNeeded()) return;
		closeDrawer();
		groupDrawerTarget = target;
		groupDrawerPresetConnectionId = null;
		groupDrawerRequestDelete = true;
		groupDrawerReadOnly = false;
		groupDrawerOpen = true;
	}

	function closeGroupDrawer(): void {
		groupDrawerOpen = false;
	}

	async function handleGroupDrawerSaved(): Promise<void> {
		await reload();
	}

	async function handleGroupDrawerDeleted(): Promise<void> {
		await reload();
	}

	/**
	 * `createTag` はこのページ自身が持つ create Drawer を、右クリックされた
	 * グループへプリセットした状態で開く - `handleTreeContextMenu` が既に
	 * `treeFilter` をそのグループへ合わせているので、`openCreateDrawer()`
	 * （`resolveGroupIdFromTreeSelection` 経由で選択中ノードからプリセット
	 * する T18-2d 既存ロジック）をそのまま呼ぶだけでよい（T18-2e から無改変）。
	 * 接続/グループの作成・再設定・削除（T18-6d 追加分）は、いずれも
	 * `ConnectionDrawer`/`CollectionGroupDrawer` を対応するモードで開く
	 * 上記の open系/Flow系関数へ振り分けるだけで、独自の CRUD ロジックは持たない。
	 * `viewConnection`/`viewGroup`（T19 S1-a 追加分、viewer 向け）も同じ
	 * Drawer を `readOnly` モードで開く `openConnectionViewDrawer`/
	 * `openGroupViewDrawer` へ振り分けるだけ - 新しい画面や別実装は持たない。
	 */
	function activateTreeContextMenuAction(action: TreeContextMenuItemAction): void {
		switch (action.kind) {
			case 'createTag':
				openCreateDrawer();
				break;
			case 'createConnection':
				openConnectionCreateDrawer();
				break;
			case 'createGroup':
				openGroupCreateDrawer(action.connectionId);
				break;
			case 'reconfigureConnection':
				openConnectionEditDrawer(action.connectionId);
				break;
			case 'deleteConnection':
				openConnectionDeleteFlow(action.connectionId);
				break;
			case 'reconfigureGroup':
				openGroupEditDrawer(action.groupId);
				break;
			case 'deleteGroup':
				openGroupDeleteFlow(action.groupId);
				break;
			case 'viewConnection':
				openConnectionViewDrawer(action.connectionId);
				break;
			case 'viewGroup':
				openGroupViewDrawer(action.groupId);
				break;
		}
	}

	/**
	 * T18-2d（TAG-UX-A、collection-groups ページの「次へ: タグを登録」CTA の
	 * 受け側）: `/tags?groupId=` で渡された収集グループをツリー選択へ反映
	 * する（一度だけ - `onboardingQueryApplied` で guard、`reload()` は
	 * 作成/削除のたびにも呼ばれるため `groups`/`connections` の参照は何度も
	 * 変わる）。ツリー選択に反映するだけで Drawer は自動で開かない -
	 * 「新規登録」ボタンを押した時点で `openCreateDrawer()` がこの選択から
	 * プリセットする（同関数の doc comment 参照）。calc/mem 配下や無効な ID
	 * は `resolvePresetGroupId` が弾くのでツリー選択は変えない。
	 */
	let onboardingQueryApplied = $state(false);
	$effect(() => {
		if (onboardingQueryApplied) return;
		if (connections.length === 0 && groups.length === 0) return;
		onboardingQueryApplied = true;
		const id = resolvePresetGroupId(page.url.searchParams.get('groupId'), groups, connections);
		if (id !== null) treeFilter = { type: 'group', id };
	});

	const filteredTags = $derived.by((): Tag[] => {
		let list = tags;
		if (treeFilter.type === 'group') {
			const groupId = treeFilter.id;
			list = list.filter((t) => t.collectionGroupId === groupId);
		} else if (treeFilter.type === 'connection') {
			const connectionId = treeFilter.id;
			const groupIds = new Set(
				groups.filter((g) => g.plcConnectionId === connectionId).map((g) => g.id)
			);
			list = list.filter((t) => groupIds.has(t.collectionGroupId));
		}
		const q = searchQuery.trim().toLowerCase();
		if (q !== '') {
			list = list.filter(
				(t) => t.name.toLowerCase().includes(q) || t.address.toLowerCase().includes(q)
			);
		}
		return list;
	});

	/**
	 * T18-3e: BantoGrid に渡す実際の行。保存前の保留編集を可視化するため、
	 * `filteredTags` に保留中の上書きをローカルに重ねる（実装指示「保留中は
	 * グリッド表示に反映する工夫」）。BantoGrid 自身は `rows` を書き換えない
	 * ので、これをしないと `onCellEdit` の commit 完了直後にセルが元の値へ
	 * 見た目上「戻って」しまう（チェックボックスをオンにした直後にオフへ
	 * 戻って見える等、実運用上ほぼバグにしか見えないため上書き表示にした）。
	 */
	const gridDisplayRows = $derived(
		pendingCellOverridesById.size === 0
			? filteredTags
			: filteredTags.map((t) => applyTagCellOverrides(t, pendingCellOverridesById.get(t.id)))
	);

	// --- T11-1: 連続登録 (docs/ux-plan.md §3) ------------------------------
	//
	// 名前パターン・開始番号・開始アドレス・点数・共通設定から
	// `generateContinuousTags`（純関数、$lib/banto/continuousRegistration.ts）
	// でプレビュー行を組み立て、確認後に一括 API を叩く。連続登録は PLC
	// アドレスを前提とする機能のため tagKind は常に 'plc'（TagInput 側の
	// 既定と同じ、フォーム自体に種別選択は出さない）。
	//
	// T19 S1-b（UX-35、docs/banto-hub-t19-design.md §2「名前パターンの既定を
	// デバイス名から導出・開始番号は入力不要」、2026-09-02 オーナー決定）:
	// `namePattern`/`startNumber` はもう固定の初期値（旧 `temp{n}`/`1`）を
	// 持たず空で始め、開始アドレス欄の入力に追従して
	// `nextNamePatternOnAddressChange`/`nextStartNumberOnAddressChange`
	// （touched 追跡方式、`$lib/banto/tagNamePrefill.ts::
	// nextTagNameOnAddressChange` と同じ設計）でプリフィルする。

	function blankContinuousForm(): ContinuousFormState {
		return {
			collectionGroupId: '',
			namePattern: '',
			startNumber: '',
			startAddress: '',
			count: '1',
			dataType: 'i16',
			stringLength: '',
			unit: '',
			decimals: '0',
			rawLo: '',
			rawHi: '',
			engLo: '',
			engHi: '',
			thresholdH: '',
			thresholdHh: '',
			thresholdL: '',
			thresholdLl: '',
			enabled: true,
			writable: false
		};
	}

	let continuousForm = $state(blankContinuousForm());
	/** T18-1（TAG-UX-C 一部）: `createBaseline` と同じ役割、連続登録版。 */
	let continuousBaseline: ContinuousFormState = blankContinuousForm();

	/**
	 * T19 S1-b（UX-35）: 名前パターン欄・開始番号欄をユーザーが直接編集した
	 * 合図。`createNameTouched`（`tagNamePrefill.ts` 参照）と同じ役割 -
	 * 立っている間は開始アドレス入力に追従させない。
	 *
	 * **`openContinuousDrawer` では false へ戻さない**: 連続登録フォーム
	 * 自体（`continuousForm`）は Drawer を閉じて開き直しても値を保持する
	 * 既存の挙動（`openContinuousDrawer` が `continuousBaseline` だけ
	 * 差し替え、`continuousForm` はリセットしない — 下の関数のコメント
	 * 参照）に合わせ、ここも同じタイミング（一括登録が成功して
	 * `continuousForm` 自体が空へ戻る `handleApplyContinuous`）でのみ
	 * `false` に戻す。Drawer を閉じただけで touched をリセットすると、
	 * 「ユーザーが編集した名前パターンが残っているのに、次に開いたときの
	 * 最初のアドレス編集で黙って上書きされる」という事故が起きるため。
	 */
	let continuousNamePatternTouched = $state(false);
	let continuousStartNumberTouched = $state(false);

	/**
	 * T19 S1-b（UX-36、単票フォームの `createDetailOpen`/`editDetailOpen` と
	 * 同じ考え方）: 連続登録フォームの「表示・スケーリング」「しきい値」の
	 * 開閉状態。既定は閉じた状態（design「既定は閉じた状態」）。連続登録は
	 * フィールド単位のサーバーエラーを持たない（検証結果はプレビュー
	 * テーブルの行単位エラー）ため、単票フォームのような「エラー時に自動
	 * 展開」は無い。
	 */
	let continuousDetailOpen: { display: boolean; threshold: boolean } = $state({
		display: false,
		threshold: false
	});

	/** 入力が変わるたびに再計算される、適用前プレビュー(設計「適用前にプレビュー表示」)。
	 *
	 * パラメータ組み立て自体（`form.count` 等の number|null 混入への対応、
	 * TAG-P0-1）は `$lib/banto/continuousRegistration.ts` の
	 * {@link buildContinuousParams} に切り出してある。 */
	let continuousPreview: ContinuousRegistrationResult | null = $derived.by(() => {
		const params = buildContinuousParams(continuousForm);
		return params ? generateContinuousTags(params) : null;
	});

	const continuousTagsJson = $derived(
		continuousPreview?.ok ? JSON.stringify(continuousPreview.tags) : null
	);

	// dry-run 検証(サーバー側チェック — 既存タグとの重複名等、クライアント
	// 側のプレビューだけでは分からないもの)の鮮度をフォームの現在値と突き
	// 合わせる。フォームを1文字でも変えたら「登録」は無効化し、再検証を促す。
	let validatedTagsJson = $state<string | null>(null);
	let validationResult = $state<BatchTagsResult | null>(null);
	let validating = $state(false);
	let applyingContinuous = $state(false);

	const continuousValidatedFresh = $derived(
		continuousTagsJson !== null &&
			continuousTagsJson === validatedTagsJson &&
			validationResult?.ok === true
	);

	function invalidateContinuousValidation(): void {
		validatedTagsJson = null;
		validationResult = null;
	}

	async function handleValidateContinuous(): Promise<void> {
		if (!continuousPreview?.ok) return;
		validating = true;
		try {
			const result = await createTagsBatch(continuousPreview.tags, true);
			validationResult = result;
			validatedTagsJson = continuousTagsJson;
			if (result.ok) {
				toastStore.push('success', `検証OK: ${result.count}件登録できます`);
			} else {
				toastStore.push('error', 'エラーがあります。下の一覧を確認してください。');
			}
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			validating = false;
		}
	}

	async function handleApplyContinuous(): Promise<void> {
		if (!continuousPreview?.ok || !continuousValidatedFresh) return;
		applyingContinuous = true;
		try {
			const result = await createTagsBatch(continuousPreview.tags, false);
			validationResult = result;
			if (result.ok) {
				toastStore.push('success', `${result.count}件登録しました`);
				// T18-4c: 連続登録は対象グループが単一に確定しているため
				// group 絞りだけ渡す（対象タグ数が多くなりうるため focus は
				// 省略 - 実装指示どおりの妥協）。フォームをリセットする前に
				// 読む必要がある。
				monitorCtaHref = monitorHref({ groupId: Number(continuousForm.collectionGroupId) });
				continuousForm = blankContinuousForm();
				continuousBaseline = blankContinuousForm();
				// T19 S1-b（UX-35）: フォームが空へ戻るのに合わせ、touched も
				// 戻す（上の宣言のコメント参照）— そうしないと次回の開始
				// アドレス入力がプリフィルされなくなる。
				continuousNamePatternTouched = false;
				continuousStartNumberTouched = false;
				continuousDetailOpen = { display: false, threshold: false };
				invalidateContinuousValidation();
				await reload();
			} else {
				toastStore.push('error', '一部の行でエラーがあります。下の一覧を確認してください。');
			}
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			applyingContinuous = false;
		}
	}

	// --- T11-2/T18-3d: CSV エクスポート/インポート
	// (docs/ux-plan.md §3, docs/banto-hub-t18-design.md「T18-3d CSV
	// 新規/更新分離＋テンプレート」) -------------------------------------
	//
	// エクスポートはこのページが Blob/DOM 操作を担当し（`$lib/banto/tagCsv.ts`
	// はブラウザ API に依存しない純関数のまま保つ）、インポートは連続登録と
	// 同じ「プレビュー → 検証(dry-run) → 登録」の2段階フローを踏襲する。
	// T18-3d でモードを「新規追加(create)」「既存更新(update)」に分離した -
	// 新規追加は既存どおり `createTagsBatch`、既存更新は
	// `$lib/banto/tagCsvDiff.ts::classifyCsvUpdate` で分類してから
	// `updateTagsBatch`（changed 行のみ）を叩く。

	/** ローカル日付での `banto-hub-tags-YYYY-MM-DD.csv`（設計: ux-plan.md §3）。 */
	function csvExportFilename(): string {
		const now = new Date();
		const y = now.getFullYear();
		const m = String(now.getMonth() + 1).padStart(2, '0');
		const d = String(now.getDate()).padStart(2, '0');
		return `banto-hub-tags-${y}-${m}-${d}.csv`;
	}

	/**
	 * CSV テキストをファイルとしてダウンロードする共通ヘルパー（T18-3d、
	 * エクスポート/テンプレート DL/エラー行 DL の3箇所で使う同一パターンを
	 * 集約しただけ - 挙動は既存の `handleExportCsv` と同じ）。
	 */
	function downloadCsvText(csv: string, filename: string): void {
		const blob = new Blob([csv], { type: 'text/csv;charset=utf-8' });
		const url = URL.createObjectURL(blob);
		const a = document.createElement('a');
		a.href = url;
		a.download = filename;
		a.click();
		URL.revokeObjectURL(url);
	}

	/**
	 * T18-3d「出力範囲（全件/絞り込み/選択行）」。既定は既存挙動どおり全件。
	 */
	let csvExportScope: 'all' | 'filtered' | 'selected' = $state('all');

	/**
	 * 閲覧者でも実行可（`canWrite` でガードしない — 設定のバックアップ/
	 * レビューは読み取り専用の操作のため）。BOM は `exportTagsCsv` が
	 * 既に埋め込み済みなのでここで二重に付けない。
	 */
	function handleExportCsv(): void {
		const source =
			csvExportScope === 'selected'
				? selectedTags
				: csvExportScope === 'filtered'
					? filteredTags
					: tags;
		const csv = exportTagsCsv(source, connections, groups);
		downloadCsvText(csv, csvExportFilename());
	}

	/** T18-3d テンプレート DL: 列ヘッダのみの空 CSV（そのまま埋めて再アップロードできる形）。 */
	function handleDownloadCsvTemplate(): void {
		downloadCsvText(buildTagCsvTemplate(), 'banto-hub-tags-template.csv');
	}

	/** T18-3d インポートモード。既定は既存挙動と同じ「新規追加」。 */
	let csvMode: 'create' | 'update' = $state('create');

	let csvFileInputEl: HTMLInputElement | undefined = $state();
	let csvParseResult: ImportTagsCsvResult | null = $state(null);
	/**
	 * アップロードした CSV の生セル（`parseCsv` 直後、`parseTagsCsv` の
	 * バリデーション前）。T18-3d のエラー CSV 再 DL
	 * （{@link buildErrorRowsCsv} の `original` 列）を組み立てるために
	 * `lineNumber - 1` でこの配列を引く（`parseTagsCsv` の
	 * `ParsedCsvTagRow.lineNumber`/`CsvRowError.lineNumber` と同じ
	 * 「ヘッダ=1・最初のデータ行=2」の契約 — `parseCsv` が返す行配列は
	 * ヘッダを含む0起点なので `lineNumber - 1` が対応する行になる）。
	 */
	let csvRawTable: string[][] | null = $state(null);
	/** T18-3d: `checkCsvSizeLimit` で拒否された（解析すらしていない）ときのメッセージ。 */
	let csvSizeError: string | null = $state(null);
	/** T18-3d: `checkCsvRowLimit` で拒否されたときのメッセージ。 */
	let csvRowLimitError: string | null = $state(null);

	/**
	 * `csvTagsJson` を素直に `csvParseResult?.ok ? ... : null` と書くと、
	 * TypeScript の制御フロー解析が「`$state(null)` で宣言した直後の
	 * 変数は（この時点までの直線的なコード上では再代入が見えないため）
	 * 型が文字通り `null` に絞り込まれる」と判断し、`csvParseResult.rows`
	 * を `never` 上のプロパティアクセスとしてエラーにする（TS の既知の
	 * 挙動 — 実際には `handleCsvFileChange` 等のイベントハンドラ内で
	 * 再代入されるが、それらは別関数のため直線フロー解析には現れない）。
	 * 関数の引数として受け渡すと、引数は呼び出し元の絞り込み履歴を
	 * 引き継がず宣言型（`ImportTagsCsvResult | null`）から素直に絞り込め
	 * るため、これを回避できる。
	 */
	function tagsJsonFromCsvParseResult(result: ImportTagsCsvResult | null): string | null {
		return result?.ok ? JSON.stringify(result.rows.map((r) => r.tag)) : null;
	}

	const csvTagsJson = $derived(tagsJsonFromCsvParseResult(csvParseResult));

	// 連続登録と同じ鮮度追跡 - 検証後にファイルを差し替えたら「登録」を
	// 無効化し、再検証を要求する（新規追加モード）。
	let csvValidatedTagsJson = $state<string | null>(null);
	let csvValidationResult = $state<BatchTagsResult | null>(null);
	let csvValidating = $state(false);
	let csvApplying = $state(false);

	const csvValidatedFresh = $derived(
		csvTagsJson !== null && csvTagsJson === csvValidatedTagsJson && csvValidationResult?.ok === true
	);

	/**
	 * T18-3d 既存更新モードの分類結果。`csvMode === 'update'` かつ構文的に
	 * 妥当な行が1件以上あるときだけ計算する（`tags` が変わるたびに再計算
	 * される - 一括反映/単票編集など他フローでの変更も反映される）。
	 */
	const csvUpdateClassification = $derived.by((): CsvUpdateClassification | null => {
		if (csvMode !== 'update' || !csvParseResult?.ok || csvParseResult.rows.length === 0) {
			return null;
		}
		return classifyCsvUpdate(csvParseResult.rows, tags);
	});

	/** プレビュー表の「行番号 → changed 行」引き当て用（エラー表示の index→行番号変換に使う）。 */
	const csvUpdateChangedRows = $derived(
		csvUpdateClassification?.rows.filter((r) => r.category === 'changed') ?? []
	);

	const csvUpdateRowsJson = $derived(
		csvUpdateClassification ? JSON.stringify(csvUpdateClassification.updateRows) : null
	);
	let csvUpdateValidatedRowsJson = $state<string | null>(null);
	let csvUpdateValidationResult = $state<BatchTagsUpdateResult | null>(null);
	let csvUpdateValidating = $state(false);
	let csvUpdateApplying = $state(false);

	const csvUpdateValidatedFresh = $derived(
		csvUpdateRowsJson !== null &&
			csvUpdateRowsJson === csvUpdateValidatedRowsJson &&
			csvUpdateValidationResult?.ok === true
	);

	/** 新規追加/既存更新どちらの検証結果も無効化する（ファイル差し替え・モード切替の共通後始末）。 */
	function invalidateCsvValidation(): void {
		csvValidatedTagsJson = null;
		csvValidationResult = null;
		csvUpdateValidatedRowsJson = null;
		csvUpdateValidationResult = null;
	}

	function resetCsvImport(): void {
		csvParseResult = null;
		csvRawTable = null;
		csvSizeError = null;
		csvRowLimitError = null;
		invalidateCsvValidation();
		if (csvFileInputEl) csvFileInputEl.value = '';
	}

	/** T18-3d モード切替 - ファイルはそのまま保持し、検証結果だけ無効化する（プレビューはモードごとに再計算される）。 */
	function handleCsvModeChange(mode: 'create' | 'update'): void {
		csvMode = mode;
		invalidateCsvValidation();
	}

	/**
	 * T18-3d 受け入れ「上限超過は解析前に理由付き拒否」:
	 * `checkCsvSizeLimit` は `file.text()` の前（＝解析コストをかける前）に
	 * 呼ぶ。行数上限（`checkCsvRowLimit`）はパース後にしか分からないので
	 * `parseTagsCsv` の後に判定する。
	 */
	async function handleCsvFileChange(e: Event): Promise<void> {
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;

		csvSizeError = null;
		csvRowLimitError = null;
		csvRawTable = null;
		csvParseResult = null;
		invalidateCsvValidation();

		const sizeCheck = checkCsvSizeLimit(file.size);
		if (!sizeCheck.ok) {
			csvSizeError = sizeCheck.message;
			return;
		}

		const text = await file.text();
		// エラー CSV 再 DL の `original` 列用に、バリデーション前の生セルも
		// 保持しておく（`parseTagsCsv` はバリデーション結果だけを返す）。
		csvRawTable = parseCsv(stripBom(text));

		const result = parseTagsCsv(text, connections, groups);
		if (result.ok) {
			const rowCheck = checkCsvRowLimit(result.rows.length);
			if (!rowCheck.ok) {
				csvRowLimitError = rowCheck.message;
				return;
			}
		}
		csvParseResult = result;
	}

	/**
	 * T18-3d エラー行 CSV 再 DL。parse エラー（構文/必須項目/型不正）と
	 * 既存更新モードの分類エラー（CSV 内重複キー）のどちらか一方だけが
	 * 同時に存在しうる（分類は構文的に妥当な行にしか行わないため）。
	 */
	function handleDownloadCsvErrorRows(): void {
		let rows: { lineNumber: number; message: string; original?: string[] }[] = [];
		if (csvParseResult && !csvParseResult.ok) {
			rows = csvParseResult.errors.map((e) => ({
				lineNumber: e.lineNumber,
				message: e.message,
				original: csvRawTable?.[e.lineNumber - 1]
			}));
		} else if (csvUpdateClassification) {
			rows = csvUpdateClassification.rows
				.filter((r) => r.category === 'error')
				.map((r) => ({
					lineNumber: r.lineNumber,
					message: r.message ?? '',
					original: csvRawTable?.[r.lineNumber - 1]
				}));
		}
		if (rows.length === 0) return;
		downloadCsvText(buildErrorRowsCsv(rows), 'banto-hub-tags-errors.csv');
	}

	async function handleValidateCsv(): Promise<void> {
		if (!csvParseResult?.ok || csvParseResult.rows.length === 0) return;
		csvValidating = true;
		try {
			const result = await createTagsBatch(
				csvParseResult.rows.map((r) => r.tag),
				true
			);
			csvValidationResult = result;
			csvValidatedTagsJson = csvTagsJson;
			if (result.ok) {
				toastStore.push('success', `検証OK: ${result.count}件登録できます`);
			} else {
				toastStore.push('error', 'エラーがあります。下の一覧を確認してください。');
			}
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			csvValidating = false;
		}
	}

	async function handleApplyCsv(): Promise<void> {
		if (!csvParseResult?.ok || !csvValidatedFresh) return;
		csvApplying = true;
		try {
			const result = await createTagsBatch(
				csvParseResult.rows.map((r) => r.tag),
				false
			);
			csvValidationResult = result;
			if (result.ok) {
				toastStore.push('success', `${result.count}件登録しました`);
				// T18-4c: 取り込み行が単一グループに収まっていれば group 絞り、
				// 複数グループに跨る場合は絞り無し（`soleGroupId` が null を
				// 返す）で `/monitor` へ。`resetCsvImport()` で `csvParseResult`
				// が消える前に読む必要がある。
				monitorCtaHref = monitorHref({
					groupId: soleGroupId(csvParseResult.rows.map((r) => r.tag.collectionGroupId))
				});
				resetCsvImport();
				await reload();
			} else {
				toastStore.push('error', '一部の行でエラーがあります。下の一覧を確認してください。');
			}
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			csvApplying = false;
		}
	}

	/**
	 * T18-3d 既存更新モードの「検証」。`updateTagsBatch` を `dryRun: true`
	 * で `changed` 行のみ呼ぶ（`updateRows` が既に changed だけを含む）。
	 */
	async function handleValidateCsvUpdate(): Promise<void> {
		if (!csvUpdateClassification || csvUpdateClassification.updateRows.length === 0) return;
		csvUpdateValidating = true;
		try {
			const result = await updateTagsBatch(csvUpdateClassification.updateRows, true);
			csvUpdateValidationResult = result;
			csvUpdateValidatedRowsJson = csvUpdateRowsJson;
			if (result.ok) {
				toastStore.push('success', `検証OK: ${result.count}件更新できます`);
			} else {
				toastStore.push('error', 'エラーがあります。下の一覧を確認してください。');
			}
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			csvUpdateValidating = false;
		}
	}

	/**
	 * T18-3d 既存更新モードの適用。CSV に無い既存タグには一切触れない
	 * （`updateRows` が changed のみのため暗黙削除・暗黙新規作成は発生しない
	 * - `tagCsvDiff.ts` の doc comment 参照）。
	 */
	async function handleApplyCsvUpdate(): Promise<void> {
		if (!csvUpdateClassification || !csvUpdateValidatedFresh) return;
		csvUpdateApplying = true;
		try {
			const result = await updateTagsBatch(csvUpdateClassification.updateRows, false);
			csvUpdateValidationResult = result;
			if (result.ok) {
				toastStore.push('success', `${result.count}件更新しました`);
				// T18-4c: 新規CSVと同じ考え方 - 更新行が単一グループに収まって
				// いれば group 絞り、複数グループ跨ぎなら絞り無し。
				// `resetCsvImport()` の前に `csvUpdateClassification` を読む。
				monitorCtaHref = monitorHref({
					groupId: soleGroupId(csvUpdateClassification.updateRows.map((r) => r.collectionGroupId))
				});
				resetCsvImport();
				await reload();
			} else {
				toastStore.push(
					'error',
					'一部の行で更新エラーが発生しました。下の一覧を確認してください。'
				);
			}
		} catch (err) {
			// T18-3b の一括反映と同じく、稼働中の 202 キュー投入
			// (QueuedWhileRunningError) も含めて汎用エラートーストに委ねる。
			toastStore.push('error', errorMessage(err));
		} finally {
			csvUpdateApplying = false;
		}
	}

	/** T18-3d 差分プレビュー表の区分ラベル。 */
	function csvUpdateCategoryLabel(category: CsvRowCategory): string {
		switch (category) {
			case 'added':
				return '追加';
			case 'changed':
				return '変更';
			case 'unchanged':
				return '変更なし';
			case 'error':
				return 'エラー';
		}
	}

	/**
	 * T18-3d 差分プレビュー表の「内容」列（changed 行）。`FIELD_LABELS` で
	 * 日本語化し、無ければ raw キーのまま表示する。
	 */
	function formatCsvUpdateDiffs(diffs: CsvUpdateRow['diffs']): string {
		if (!diffs || diffs.length === 0) return '';
		return diffs.map((d) => `${FIELD_LABELS[d.field] ?? d.field}: ${d.from} → ${d.to}`).join(', ');
	}

	// --- T18-3b: 一括操作 (docs/banto-hub-t18-design.md「T18-3b 一括操作」) --
	//
	// 連続登録・CSVインポートの「プレビュー→確認→適用」の流儀を踏襲するが、
	// ここでは対象件数・差分の計算はサーバー往復なしのクライアント純関数
	// （`$lib/banto/tagBulkOps.ts::summarizeBulkChange`）で行う - 選択済み
	// タグの現在値は既にこのページの `tags` にあるため、dry-run を別途
	// 叩かなくても「対象N件・差分」を確認パネルに出せる（実装指示「過剰
	// 実装は避け、最低限『件数＋主要差分を見せてから適用』を満たす」）。

	/** 一括操作バーの3ボタンに対応する。`null` は確認パネルを閉じている状態。 */
	type BulkAction = 'enable' | 'disable' | 'move' | null;
	let bulkAction: BulkAction = $state(null);
	/** `bulkAction === 'move'` のときの移動先グループ（`<select>` の値、未選択は `''`）。 */
	let bulkTargetGroupId = $state('');
	let bulkApplying = $state(false);
	/** 直近の `updateTagsBatch` 応答。`ok: false` のときだけ行エラー表示に使う。 */
	let bulkResult: BatchTagsUpdateResult | null = $state(null);

	const selectedTags = $derived(tags.filter((t) => selectedIds.has(t.id)));
	/** T18-3b「選択タグに複数種別混在の場合はグループ移動を無効化」の判定。有効/無効切替は種別混在でも可。 */
	const selectedTagsMixedKind = $derived(hasMixedTagKinds(selectedTags));

	/** 移動先候補 - 選択タグの種別（混在していなければ）に整合するグループのみ（`groupsFor` を再利用）。 */
	const bulkMoveGroupOptions = $derived.by((): CollectionGroup[] => {
		if (selectedTags.length === 0 || selectedTagsMixedKind) return [];
		return groupsFor(selectedTags[0].tagKind);
	});

	const bulkTargetGroupIdNum = $derived.by((): number | null => {
		if (bulkTargetGroupId === '') return null;
		const n = Number(bulkTargetGroupId);
		return Number.isFinite(n) ? n : null;
	});

	/** `updateTagsBatch` に渡す実際の行。移動先未選択など未確定の間は空配列（「適用」を無効化する判定にも使う）。 */
	const bulkRows = $derived.by((): BatchTagUpdateRow[] => {
		if (bulkAction === 'enable') return buildBulkEnableRows(selectedTags, true);
		if (bulkAction === 'disable') return buildBulkEnableRows(selectedTags, false);
		if (bulkAction === 'move' && bulkTargetGroupIdNum !== null) {
			return buildBulkMoveRows(selectedTags, bulkTargetGroupIdNum);
		}
		return [];
	});

	/** 確認パネルの「対象N件・差分」表示。`bulkRows` と同じ確定条件（移動先未選択なら `null`）。 */
	const bulkSummary = $derived.by((): BulkChangeSummary<boolean | number> | null => {
		if (bulkAction === 'enable') return summarizeBulkChange(selectedTags, 'enabled', true);
		if (bulkAction === 'disable') return summarizeBulkChange(selectedTags, 'enabled', false);
		if (bulkAction === 'move' && bulkTargetGroupIdNum !== null) {
			return summarizeBulkChange(selectedTags, 'collectionGroupId', bulkTargetGroupIdNum);
		}
		return null;
	});

	/** 差分テーブルの「変更前/変更後」セル表示 - `enabled` は日本語ラベル、`collectionGroupId` はグループ名に整形する。 */
	function bulkFieldDisplay(action: Exclude<BulkAction, null>, value: boolean | number): string {
		if (action === 'move') return groupName(Number(value));
		return value ? '有効' : '無効';
	}

	function openBulkPanel(action: 'enable' | 'disable' | 'move'): void {
		bulkAction = action;
		bulkTargetGroupId = '';
		bulkResult = null;
	}

	function closeBulkPanel(): void {
		bulkAction = null;
		bulkTargetGroupId = '';
		bulkResult = null;
	}

	/**
	 * 「この内容で一括反映」— `dryRun: false` で直接適用する（上のコメント
	 * のとおり、対象件数・差分は既にクライアント側で確認済みという設計）。
	 * `errors` が返れば（`ok: false`）確認パネルは開いたまま行エラーを
	 * 表示し、選択・入力はそのまま保持する（連続登録/CSVの「検証」失敗時と
	 * 同じ「直さず再送信できる」形）。
	 */
	async function handleApplyBulk(): Promise<void> {
		if (bulkAction === null || bulkRows.length === 0) return;
		bulkApplying = true;
		bulkResult = null;
		try {
			const result = await updateTagsBatch(bulkRows, false);
			bulkResult = result;
			if (result.ok) {
				toastStore.push('success', `選択した${result.count}件を一括反映しました`);
				// T18-4c: move は移動先グループが確定しているのでそれを使う。
				// enable/disable は選択タグ群のグループが単一なら絞り、複数
				// グループへ跨るなら絞り無し。`closeBulkPanel()`/`selectedIds`
				// リセットの前に `selectedTags`（選択中の $derived）を読む。
				monitorCtaHref =
					bulkAction === 'move' && bulkTargetGroupIdNum !== null
						? monitorHref({ groupId: bulkTargetGroupIdNum })
						: monitorHref({ groupId: soleGroupId(selectedTags.map((t) => t.collectionGroupId)) });
				closeBulkPanel();
				selectedIds = new Set();
				await reload();
			} else {
				toastStore.push('error', '選択タグの一部で更新エラーが発生しました（下の一覧参照）。');
			}
		} catch (err) {
			// T18-3b（収集稼働中の 202 キュー投入）: `QueuedWhileRunningError` も
			// 含め、他の書き込み系呼び出しと同じ汎用エラートーストに委ねる -
			// 現状の単票/連続登録/CSVインポートもこの経路以上の専用UIは
			// 持っていないため、ここだけ特別扱いはしない。
			toastStore.push('error', errorMessage(err));
		} finally {
			bulkApplying = false;
		}
	}

	/**
	 * T18-3e: `gridEditMode`（表編集モード）に応じて列定義を差し替える
	 * `$derived.by` - プレーンな `const` のままだと、`editable`/`editor` を
	 * 常に持たせておいて評価結果だけ `gridEditMode` 次第で切り替える実装に
	 * なりがちだが、それは誤り。BantoGrid の `hasEditableColumns` は
	 * `columns.some(c => Boolean(c.editable))`（**関数さえ入っていれば
	 * true** - 呼び出した結果は見ない）で決まり、これが true になると
	 * 単一クリックの `onRowClick` が発火しなくなりダブルクリックへ切り替わる
	 * （`node_modules/@banto/grid-svelte` の `BantoGrid.svelte`
	 * `hasEditableColumns`/`handleCellClick` 参照）。
	 *
	 * そのため OFF（既定）のときは `editable` キー自体を持たせない8列
	 * （既存どおり）を返し、単一クリック編集・複数選択（T18-3b）を厳密に
	 * 維持する。ON のときだけ `enabled`/`writable` に `editable`/`editor` を
	 * 足し、`unit`/`decimals` の2列を追加する（実装指示「編集モード時のみ
	 * 列追加」- 既存 e2e はいずれも表編集モードへ入らないため、この2列は
	 * 既存 spec のグリッド構造に一切影響しない）。
	 *
	 * `address`/`dataType`/`collectionGroupId`/`tagKind`/`name`/`expression`
	 * は意図的に対象外（型連動・配置規則・DAG・一意制約・外部名変更の連鎖が
	 * 重く、単票 Drawer に誘導する設計判断、実装指示 T18-3e 参照）。
	 *
	 * 各 editable 列の `editable` は関数形 `() => gridEditMode &&
	 * collectionStopped` - 「表編集モードON」かつ「収集停止中」の両方を
	 * 満たす間だけ実際に編集できる（停止中ロック）。
	 */
	const columns = $derived.by((): GridColumn<Tag>[] => {
		const cellEditable = () => gridEditMode && collectionStopped;
		const base: GridColumn<Tag>[] = [
			{ id: 'id', header: 'ID', accessor: 'id', width: 60, align: 'right' },
			{
				id: 'name',
				header: '名前',
				accessor: 'name',
				width: 140,
				filterable: true,
				filterType: 'text'
			},
			{
				id: 'collectionGroupId',
				header: '収集グループ',
				accessor: (row) => groupName(row.collectionGroupId),
				width: 140
			},
			{ id: 'address', header: 'アドレス', accessor: 'address', width: 100 },
			{ id: 'dataType', header: '型', accessor: 'dataType', width: 80 },
			{
				id: 'tagKind',
				header: '種別',
				accessor: 'tagKind',
				width: 90
			},
			{
				id: 'enabled',
				header: '有効',
				accessor: 'enabled',
				width: 70,
				format: (v) => (v ? 'はい' : 'いいえ'),
				...(gridEditMode ? { editable: cellEditable, editor: 'checkbox' as const } : {})
			},
			{
				id: 'writable',
				header: '書き込み可',
				accessor: 'writable',
				width: 90,
				format: (v) => (v ? 'はい' : 'いいえ'),
				...(gridEditMode ? { editable: cellEditable, editor: 'checkbox' as const } : {})
			}
		];
		if (!gridEditMode) return base;
		return [
			...base,
			{
				id: 'unit',
				header: '単位',
				accessor: 'unit',
				width: 90,
				editable: cellEditable,
				editor: 'text' as const
			},
			{
				id: 'decimals',
				header: '小数桁数',
				accessor: 'decimals',
				width: 90,
				editable: cellEditable,
				editor: 'number' as const,
				validate: (value: unknown) => {
					const n = Number(value);
					if (!Number.isInteger(n) || n < MIN_DECIMALS || n > MAX_DECIMALS) {
						return `小数桁数は ${MIN_DECIMALS}〜${MAX_DECIMALS} の整数で指定してください`;
					}
					return null;
				}
			}
		];
	});
</script>

{#snippet tagFields(
	form: FormState,
	errors: Record<string, string>,
	detailOpen: DetailOpenState,
	addressPreflight: AddressPreflightState,
	onAddressInput: () => void,
	onNameInput: () => void,
	onWritableInput: () => void
)}
	<!--
		TAG-P0-2（docs/banto-hub-desktop-plan.md §9.3、2026-08-10 実装メモ）:
		preflight 失敗（field="configuration"）はどの単票フィールドにも
		属さないフォーム全体エラーのため、個別フィールドの並ぶ form-grid
		の外（上）に置く。create/edit 両方の form がこの snippet を
		render するので、ここ1箇所で両方をカバーする。
	-->
	{#if errors.configuration}
		<p class="err" role="alert">{errors.configuration}</p>
	{/if}
	<!--
		T18-2a（docs/banto-hub-t18-design.md「T18-2a 単票フォーム刷新」、
		TAG-UX-B「入力順を『タグ種別 → 接続／グループ → 名前 → アドレス →
		データ型』とする」）: 常時表示の基本設定。既存フィールドの入力順
		だけを並べ替えたもので、各フィールドの挙動（`required`・
		`groupsFor` 絞り込み・ヒント文言）は元のまま変更していない。
	-->
	<div class="form-grid">
		<label class="field">
			タグ種別
			<select
				id="tag-kind"
				bind:value={form.tagKind}
				aria-invalid={errors.tagKind ? 'true' : undefined}
				aria-describedby={describedBy(errors.tagKind && 'tag-kind-err')}
				onchange={() => {
					// タグ種別を切り替えたら、もう選択できないグループ ID は
					// クリアする（`groupsFor` の絞り込みと矛盾する選択を残さない）。
					if (!groupsFor(form.tagKind).some((g) => String(g.id) === form.collectionGroupId)) {
						form.collectionGroupId = '';
					}
				}}
			>
				{#each TAG_KIND_OPTIONS as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.tagKind}<span class="err" id="tag-kind-err">{errors.tagKind}</span>{/if}
		</label>
		<label class="field">
			収集グループ<span class="required">*</span>
			<select
				id="tag-group"
				bind:value={form.collectionGroupId}
				required
				aria-invalid={errors.collectionGroupId ? 'true' : undefined}
				aria-describedby={describedBy(
					(form.tagKind !== 'plc' || form.collectionGroupId !== '') && 'tag-group-hint',
					errors.collectionGroupId && 'tag-group-err'
				)}
			>
				<option value="" disabled>選択してください</option>
				{#each groupsFor(form.tagKind) as group (group.id)}
					<option value={String(group.id)}>{group.name}</option>
				{/each}
			</select>
			{#if form.tagKind === 'computed'}
				<span class="hint" id="tag-group-hint"
					>{CALC_CONNECTION_NAME} 接続配下のグループのみ選択できます。</span
				>
			{:else if form.tagKind === 'internal'}
				<span class="hint" id="tag-group-hint"
					>{MEM_CONNECTION_NAME} 接続配下のグループのみ選択できます。</span
				>
			{:else if form.tagKind === 'plc' && form.collectionGroupId !== ''}
				<!--
					T18-2a（TAG-UX-B 拡張）: plc タグは収集グループから接続が
					一意に決まるため、選択済みグループがどの接続配下か・実機か
					SIM かをここで先出しする（保存前の確認領域とは別に、選択
					直後にも分かるようにする）。
				-->
				<span class="hint" id="tag-group-hint"
					>接続: {connectionForGroupId(form.collectionGroupId)?.name}（{confirmEnvironmentLabel(
						form
					)}）</span
				>
			{/if}
			{#if errors.collectionGroupId}<span class="err" id="tag-group-err"
					>{errors.collectionGroupId}</span
				>{/if}
		</label>
		<label class="field">
			名前<span class="required">*</span>
			<input
				id="tag-name"
				type="text"
				bind:value={form.name}
				oninput={onNameInput}
				required
				aria-invalid={errors.name ? 'true' : undefined}
				aria-describedby={describedBy(errors.name && 'tag-name-err')}
			/>
			{#if errors.name}<span class="err" id="tag-name-err">{errors.name}</span>{/if}
		</label>
		{#if form.tagKind === 'plc'}
			<!--
				T18-2b（docs/banto-hub-t18-design.md「T18-2b プロトコル別アドレス
				補助」、TAG-UX-6）: 選択中の収集グループが属する接続の
				`protocol`（`slmp`/`modbus-tcp`/未選択）と `dataType` から、
				アドレス例・対応デバイス・占有範囲・bit 指定可否をここで
				切り替える。マッピング自体は依存ゼロの純関数
				`$lib/banto/tagAddressHelp.ts::addressHelpFor` に切り出してあり
				（受け入れ条件「Modbus 選択時に D100 を推奨例にしない」は
				そちら側のユニットテストで固定）、ここは表示だけを担う。
				`preflightFieldErrors`/`preflightMessage` は入力中の
				デバウンス dry-run 検証（`scheduleAddressPreflight`/
				`runAddressPreflight`、同じ preflight 契約
				`createTagsBatch(..., dryRun=true)` を流用）の表示 - 送信時の
				`errors.address`（`tag-address-err`）とは別枠で、送信前の
				参考表示として `tag-address-preflight` に出す。

				他フィールドと違いここだけ `<label class="field">` で
				input を包まず `<div class="field">` + 明示 `<label for>` に
				している - アドレス例・デバイス一覧・bit 指定ヒントの文章量が
				多く、暗黙ラベル（label の子孫テキスト全体が accessible name
				になる）のままだと「収集グループ」「単位」等、他フィールドの
				ラベル文言をヒント文中にたまたま含んだ瞬間 `getByLabel` が
				両方にマッチしてしまう（実 DOM 検証で発覚 - ヒント文に
				「収集グループ」を含めた版で
				`banto-hub-tags-form.spec.ts`/`banto-hub-tags-p0-2-preflight.spec.ts`
				が、「ビット単位」を含めた版で `banto-hub-tags-revision.spec.ts`
				がそれぞれ多重マッチで落ちた）。`<label for>` で名前を
				「アドレス」だけに固定すれば、ヒント文の言葉選びに関わらず
				安全。
			-->
			{@const protocol = connectionForGroupId(form.collectionGroupId)?.protocol}
			{@const addressHelp = addressHelpFor(protocol, form.dataType)}
			{@const preflightFieldErrors = addressPreflight.result
				? fieldErrorsFromList(addressPreflight.result.errors[0]?.fieldErrors ?? [])
				: {}}
			{@const preflightMessage = addressPreflight.checking
				? '確認中…'
				: addressPreflight.result?.ok
					? '検証OK'
					: (preflightFieldErrors.address ?? null)}
			<div class="field">
				<label for="tag-address">アドレス<span class="required">*</span></label>
				<input
					id="tag-address"
					type="text"
					bind:value={form.address}
					oninput={onAddressInput}
					required
					placeholder={addressHelp.placeholder}
					aria-invalid={errors.address ? 'true' : undefined}
					aria-describedby={describedBy(
						'tag-address-hint',
						addressHelp.examples.length > 0 && 'tag-address-examples',
						'tag-address-bit-hint',
						preflightMessage !== null && 'tag-address-preflight',
						errors.address && 'tag-address-err'
					)}
				/>
				<span class="hint" id="tag-address-hint"
					>{addressHelp.deviceHint}{#if addressHelp.occupancyHint !== ''}
						{addressHelp.occupancyHint}{/if}</span
				>
				{#if addressHelp.examples.length > 0}
					<span class="hint" id="tag-address-examples"
						>例: {addressHelp.examples
							.map((e) => `${e.address}（${e.description}）`)
							.join('、')}</span
					>
				{/if}
				<span class="hint" id="tag-address-bit-hint">{addressHelp.bitHint}</span>
				{#if preflightMessage !== null}
					<span
						class="hint address-preflight"
						class:address-preflight-checking={addressPreflight.checking}
						class:address-preflight-ok={addressPreflight.result?.ok === true}
						class:address-preflight-error={addressPreflight.result?.ok === false}
						id="tag-address-preflight"
						aria-live="polite">{preflightMessage}</span
					>
				{/if}
				{#if errors.address}<span class="err" id="tag-address-err">{errors.address}</span>{/if}
			</div>
		{/if}
		{#if form.tagKind === 'computed'}
			<label class="field wide">
				式（expression）<span class="required">*</span>
				<textarea
					id="tag-expression"
					bind:value={form.expression}
					rows="2"
					required
					placeholder="(line1.fast.a + line1.fast.b) / 2"
					aria-invalid={errors.expression ? 'true' : undefined}
					aria-describedby={describedBy(
						'tag-expression-hint',
						errors.expression && 'tag-expression-err'
					)}></textarea>
				<span class="hint" id="tag-expression-hint"
					>四則・比較・論理・if(c,a,b)・min/max/abs/round/clamp/bit(tag,n)。参照する外部名は他タグ
					（plc/computed/internal）の完全名。</span
				>
				{#if errors.expression}<span class="err" id="tag-expression-err">{errors.expression}</span
					>{/if}
			</label>
		{/if}
		<label class="field">
			データ型
			<select
				id="tag-data-type"
				bind:value={form.dataType}
				aria-invalid={errors.dataType ? 'true' : undefined}
				aria-describedby={describedBy(errors.dataType && 'tag-data-type-err')}
			>
				{#each dataTypeOptions as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.dataType}<span class="err" id="tag-data-type-err">{errors.dataType}</span>{/if}
		</label>
		{#if form.dataType === 'string'}
			<label class="field">
				文字列長（word数）
				<input
					id="tag-string-length"
					type="number"
					min={MIN_STRING_LENGTH}
					max={MAX_STRING_LENGTH}
					bind:value={form.stringLength}
					aria-invalid={errors.stringLength ? 'true' : undefined}
					aria-describedby={describedBy(
						'tag-string-length-hint',
						errors.stringLength && 'tag-string-length-err'
					)}
				/>
				<span class="hint" id="tag-string-length-hint"
					>{MIN_STRING_LENGTH}〜{MAX_STRING_LENGTH} word（1 word = 2バイト）。</span
				>
				{#if errors.stringLength}<span class="err" id="tag-string-length-err"
						>{errors.stringLength}</span
					>{/if}
			</label>
		{/if}
		<label class="field">
			単位
			<input
				id="tag-unit"
				type="text"
				bind:value={form.unit}
				placeholder="℃"
				aria-invalid={errors.unit ? 'true' : undefined}
				aria-describedby={describedBy(errors.unit && 'tag-unit-err')}
			/>
			{#if errors.unit}<span class="err" id="tag-unit-err">{errors.unit}</span>{/if}
		</label>
		<label class="field checkbox">
			<input id="tag-enabled" type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
		{#if form.tagKind === 'internal'}
			<label class="field checkbox">
				<input id="tag-retain" type="checkbox" bind:checked={form.retain} />
				再起動時に最終値を復元（retain）
			</label>
		{/if}
	</div>
	<!--
		T18-2a（TAG-UX-B「常用する基本設定と、詳細設定を fieldset /
		折りたたみで分ける」）: 表示・スケーリングの詳細。`RawLo`/`RawHi`/
		`EngLo`/`EngHi` は「入力下限 (RawLo)」のように日本語ラベルを先出し
		する（TAG-UX-B「『入力下限 (RawLo)』のように日本語を先に表示す
		る」）。`detailOpen.display` が false でも値自体は `form` に残り
		続ける（`<details>` の開閉は表示のみを制御し、束縛先の `form.xxx`
		は変わらない）。
	-->
	<details class="detail-group" bind:open={detailOpen.display}>
		<summary>
			表示・スケーリング
			{#if hasAnyFieldValue(form, DISPLAY_SCALING_VALUE_FIELDS)}
				<!--
					T19 S1-b（UX-36「値が設定されているときは、閉じていてもそれが
					分かるようにする」、2026-09-02 オーナー決定）: RawLo/Hi・
					EngLo/Hi のいずれかに値が入っていれば、詳細を開かなくても
					分かるバッジを summary に出す。閉じたまま気付けないと危険
					（design原文）という安全上の理由。
				-->
				<span class="detail-value-badge" title="値が設定されています">設定あり</span>
			{/if}
		</summary>
		<div class="form-grid">
			<label class="field">
				小数桁数
				<input
					id="tag-decimals"
					type="number"
					min="0"
					bind:value={form.decimals}
					aria-invalid={errors.decimals ? 'true' : undefined}
					aria-describedby={describedBy(errors.decimals && 'tag-decimals-err')}
				/>
				{#if errors.decimals}<span class="err" id="tag-decimals-err">{errors.decimals}</span>{/if}
			</label>
			<label class="field">
				入力下限 (RawLo)
				<input
					id="tag-raw-lo"
					type="number"
					bind:value={form.rawLo}
					aria-invalid={errors.rawLo ? 'true' : undefined}
					aria-describedby={describedBy(errors.rawLo && 'tag-raw-lo-err')}
				/>
				{#if errors.rawLo}<span class="err" id="tag-raw-lo-err">{errors.rawLo}</span>{/if}
			</label>
			<label class="field">
				入力上限 (RawHi)
				<input
					id="tag-raw-hi"
					type="number"
					bind:value={form.rawHi}
					aria-invalid={errors.rawHi ? 'true' : undefined}
					aria-describedby={describedBy(errors.rawHi && 'tag-raw-hi-err')}
				/>
				{#if errors.rawHi}<span class="err" id="tag-raw-hi-err">{errors.rawHi}</span>{/if}
			</label>
			<label class="field">
				換算下限 (EngLo)
				<input
					id="tag-eng-lo"
					type="number"
					bind:value={form.engLo}
					aria-invalid={errors.engLo ? 'true' : undefined}
					aria-describedby={describedBy(errors.engLo && 'tag-eng-lo-err')}
				/>
				{#if errors.engLo}<span class="err" id="tag-eng-lo-err">{errors.engLo}</span>{/if}
			</label>
			<label class="field">
				換算上限 (EngHi)
				<input
					id="tag-eng-hi"
					type="number"
					bind:value={form.engHi}
					aria-invalid={errors.engHi ? 'true' : undefined}
					aria-describedby={describedBy(errors.engHi && 'tag-eng-hi-err')}
				/>
				{#if errors.engHi}<span class="err" id="tag-eng-hi-err">{errors.engHi}</span>{/if}
			</label>
		</div>
	</details>
	<details class="detail-group" bind:open={detailOpen.threshold}>
		<summary>
			しきい値
			{#if hasAnyFieldValue(form, THRESHOLD_FIELDS)}
				<span class="detail-value-badge" title="値が設定されています">設定あり</span>
			{/if}
		</summary>
		<div class="form-grid">
			<label class="field">
				しきい値 H
				<input
					id="tag-threshold-h"
					type="number"
					bind:value={form.thresholdH}
					aria-invalid={errors.thresholdH ? 'true' : undefined}
					aria-describedby={describedBy(errors.thresholdH && 'tag-threshold-h-err')}
				/>
				{#if errors.thresholdH}<span class="err" id="tag-threshold-h-err">{errors.thresholdH}</span
					>{/if}
			</label>
			<label class="field">
				しきい値 HH
				<input
					id="tag-threshold-hh"
					type="number"
					bind:value={form.thresholdHh}
					aria-invalid={errors.thresholdHh ? 'true' : undefined}
					aria-describedby={describedBy(errors.thresholdHh && 'tag-threshold-hh-err')}
				/>
				{#if errors.thresholdHh}<span class="err" id="tag-threshold-hh-err"
						>{errors.thresholdHh}</span
					>{/if}
			</label>
			<label class="field">
				しきい値 L
				<input
					id="tag-threshold-l"
					type="number"
					bind:value={form.thresholdL}
					aria-invalid={errors.thresholdL ? 'true' : undefined}
					aria-describedby={describedBy(errors.thresholdL && 'tag-threshold-l-err')}
				/>
				{#if errors.thresholdL}<span class="err" id="tag-threshold-l-err">{errors.thresholdL}</span
					>{/if}
			</label>
			<label class="field">
				しきい値 LL
				<input
					id="tag-threshold-ll"
					type="number"
					bind:value={form.thresholdLl}
					aria-invalid={errors.thresholdLl ? 'true' : undefined}
					aria-describedby={describedBy(errors.thresholdLl && 'tag-threshold-ll-err')}
				/>
				{#if errors.thresholdLl}<span class="err" id="tag-threshold-ll-err"
						>{errors.thresholdLl}</span
					>{/if}
			</label>
		</div>
	</details>
	{#if form.tagKind !== 'computed'}
		<!--
			T19 S1-b（UX-34）: `writableDefaultBlockedReason` の第2引数
			（アドレス領域がサーバー的に書き込み可能かどうか）は
			2026-09-02 オーナー判断（S1-b0 分離）によりここでは意図的に
			省略する（`undefined` 扱い）。Modbus `1xxxx`/`3xxxx` のような
			規則をアドレス文字列から UI 側で判定すると、`banto-plc`
			（`AddressArea`）・`banto-tags`（`modbus_read_only_area`）に
			続く3つ目の手書き複製になってしまうため、プロトコル層の
			データをサーバーから受け取れるようになる別スライス S1-b0 まで
			保留する（`$lib/banto/writableDefault.ts` の doc comment
			参照）。そのため現状 `writableBlockedReason` は `tagKind ===
			'computed'` のときしか非 `null` にならない（このセクション自体
			が computed では非表示なので、実質ここには来ない） -
			チェックボックスは今のところ無効化されない。S1-b0 がアドレス
			preflight 等でこの判定結果を返すようになったら、その値を
			第2引数として渡すだけで絞り込みが有効になる。
		-->
		{@const writableBlockedReason = writableDefaultBlockedReason(form.tagKind)}
		<details class="detail-group" bind:open={detailOpen.write}>
			<summary>書き込み安全設定</summary>
			<div class="form-grid">
				<label class="field checkbox wide">
					<input
						id="tag-writable"
						type="checkbox"
						checked={writableBlockedReason === null && form.writable}
						disabled={writableBlockedReason !== null}
						onchange={(e) => {
							form.writable = (e.currentTarget as HTMLInputElement).checked;
							onWritableInput();
						}}
					/>
					外部クライアントから PLC への書き込みを許可
				</label>
				{#if writableBlockedReason}
					<!--
						T19 S1-b（UX-34「該当しない場合は既定を適用せず、なぜ
						writable にできないのかが利用者に分かる表示にしてください」、
						2026-09-02 オーナー決定）: 現状は到達しない分岐
						（computed タグはこのセクション自体が非表示 - 上の
						`{#if form.tagKind !== 'computed'}`）。S1-b0 でアドレス
						領域判定が配線されれば、読み取り専用領域のときにも
						ここへ来るようになる。
					-->
					<p class="hint wide" id="tag-writable-blocked-reason">{writableBlockedReason}</p>
				{:else if form.writable}
					<!--
						T18-2a（TAG-UX-B「書き込み許可の文言は『外部クライアントから
						PLC への書き込みを許可』とし、ON 時に安全上の影響を説明す
						る」）: 何が可能になるか（外部クライアントからの書き換え）と、
						実機設備への影響リスクの両方を明示する。
					-->
					<p class="warn wide" role="note">
						外部クライアント（OPC UA・gRPC
						等）からこのタグの値を書き換えられるようになります。誤操作や意図しない書き込みは実機の設備・工程に直接影響するため、本当に必要な範囲だけ許可してください。
					</p>
				{/if}
			</div>
		</details>
	{/if}
	<!--
		T18-2a（TAG-UX-B「保存前に最終外部名 `{connection}.{group}.{tag}`、
		実機／SIM、書き込み許可を固定領域で確認できるようにする」）:
		`confirmExternalName`/`confirmEnvironmentLabel`/`confirmWriteLabel`
		は現在の `form`（`$state`）を読むだけなので、別途 `$derived` を
		用意しなくてもスニペット本体が再実行されるたびに最新値になる。
	-->
	<div class="confirm-panel">
		<h4 class="confirm-title">保存前の確認</h4>
		<dl class="confirm-list">
			<div class="confirm-row">
				<dt>外部名</dt>
				<dd>{confirmExternalName(form)}</dd>
			</div>
			<div class="confirm-row">
				<dt>実機 / SIM</dt>
				<dd>{confirmEnvironmentLabel(form)}</dd>
			</div>
			<div class="confirm-row">
				<dt>書き込み許可</dt>
				<dd>{confirmWriteLabel(form)}</dd>
			</div>
		</dl>
	</div>
{/snippet}

{#snippet continuousCommonFields()}
	<label class="field">
		データ型
		<select bind:value={continuousForm.dataType}>
			{#each dataTypeOptions as opt (opt.value)}
				<option value={opt.value}>{opt.label}</option>
			{/each}
		</select>
	</label>
	{#if continuousForm.dataType === 'string'}
		<label class="field">
			文字列長（word数）
			<input
				type="number"
				min={MIN_STRING_LENGTH}
				max={MAX_STRING_LENGTH}
				bind:value={continuousForm.stringLength}
			/>
			<span class="hint">{MIN_STRING_LENGTH}〜{MAX_STRING_LENGTH} word。連番の増分もこの値。</span>
		</label>
	{/if}
	<label class="field">
		単位
		<input type="text" bind:value={continuousForm.unit} placeholder="℃" />
	</label>
	<label class="field">
		小数桁数
		<input type="number" min="0" bind:value={continuousForm.decimals} />
	</label>
	<label class="field checkbox">
		<input type="checkbox" bind:checked={continuousForm.enabled} />
		有効
	</label>
	<label class="field checkbox">
		<input type="checkbox" bind:checked={continuousForm.writable} />
		書き込み可（writable）
	</label>
	<!--
		T19 S1-b（UX-36、単票フォームの `<details class="detail-group">` と
		同じ扱い）: RawLo/RawHi/EngLo/EngHi・しきい値 HH/H/L/LL は既定で
		閉じ、値が入っていれば summary にバッジを出す。
	-->
	<div class="continuous-detail-wrap">
		<details class="detail-group" bind:open={continuousDetailOpen.display}>
			<summary>
				表示・スケーリング
				{#if hasAnyFieldValue(continuousForm, DISPLAY_SCALING_VALUE_FIELDS)}
					<span class="detail-value-badge" title="値が設定されています">設定あり</span>
				{/if}
			</summary>
			<div class="form-grid">
				<label class="field">
					RawLo
					<input type="number" bind:value={continuousForm.rawLo} />
				</label>
				<label class="field">
					RawHi
					<input type="number" bind:value={continuousForm.rawHi} />
				</label>
				<label class="field">
					EngLo
					<input type="number" bind:value={continuousForm.engLo} />
				</label>
				<label class="field">
					EngHi
					<input type="number" bind:value={continuousForm.engHi} />
				</label>
			</div>
		</details>
		<details class="detail-group" bind:open={continuousDetailOpen.threshold}>
			<summary>
				しきい値
				{#if hasAnyFieldValue(continuousForm, THRESHOLD_FIELDS)}
					<span class="detail-value-badge" title="値が設定されています">設定あり</span>
				{/if}
			</summary>
			<div class="form-grid">
				<label class="field">
					しきい値 H
					<input type="number" bind:value={continuousForm.thresholdH} />
				</label>
				<label class="field">
					しきい値 HH
					<input type="number" bind:value={continuousForm.thresholdHh} />
				</label>
				<label class="field">
					しきい値 L
					<input type="number" bind:value={continuousForm.thresholdL} />
				</label>
				<label class="field">
					しきい値 LL
					<input type="number" bind:value={continuousForm.thresholdLl} />
				</label>
			</div>
		</details>
	</div>
{/snippet}

{#snippet batchRowErrors(result: BatchTagsResult)}
	{#if !result.ok}
		<table class="error-table">
			<thead>
				<tr>
					<th>行</th>
					<th>項目</th>
					<th>内容</th>
				</tr>
			</thead>
			<tbody>
				{#each result.errors as rowError (rowError.index)}
					{#each rowError.fieldErrors as fe, i (i)}
						<tr>
							<td>{rowError.index + 1}</td>
							<td>{fe.field}</td>
							<td>{fe.message}</td>
						</tr>
					{/each}
				{/each}
			</tbody>
		</table>
	{/if}
{/snippet}

{#snippet bulkRowErrors(result: BatchTagsUpdateResult)}
	{#if !result.ok}
		<table class="error-table">
			<thead>
				<tr>
					<th>#</th>
					<th>ID</th>
					<th>項目</th>
					<th>内容</th>
				</tr>
			</thead>
			<tbody>
				{#each result.errors as rowError (rowError.index)}
					{#each rowError.fieldErrors as fe, i (i)}
						<tr>
							<td>{rowError.index + 1}</td>
							<td>{rowError.id}</td>
							<td>{fe.field}</td>
							<td>{fe.message}</td>
						</tr>
					{/each}
				{/each}
			</tbody>
		</table>
	{/if}
{/snippet}

{#snippet csvParseErrors(errors: CsvRowError[])}
	<table class="error-table">
		<thead>
			<tr>
				<th>行</th>
				<th>内容</th>
			</tr>
		</thead>
		<tbody>
			{#each errors as e, i (i)}
				<tr>
					<td>{e.lineNumber}</td>
					<td>{e.message}</td>
				</tr>
			{/each}
		</tbody>
	</table>
{/snippet}

{#snippet csvBatchRowErrors(result: BatchTagsResult, parsedRows: ParsedCsvTagRow[])}
	{#if !result.ok}
		<table class="error-table">
			<thead>
				<tr>
					<th>行</th>
					<th>項目</th>
					<th>内容</th>
				</tr>
			</thead>
			<tbody>
				{#each result.errors as rowError (rowError.index)}
					{#each rowError.fieldErrors as fe, i (i)}
						<tr>
							<!--
								batchRowErrors（連続登録用）とは違い、ここでの `rowError.index`
								は「CSV データ行(ヘッダ除く)の0起点位置」= `createTagsBatch` に
								送った `csvParseResult.rows` 配列の添字。連続登録の「index+1」
								（プレビュー行番号）とは意味が違うので、実際の CSV ファイル行番号
								に変換するには `parsedRows[index].lineNumber` を引く必要がある
								（`$lib/banto/tagCsv.ts::ParsedCsvTagRow.lineNumber` — ヘッダ行=1,
								最初のデータ行=2）。
							-->
							<td>{parsedRows[rowError.index]?.lineNumber ?? `#${rowError.index}`}</td>
							<td>{fe.field}</td>
							<td>{fe.message}</td>
						</tr>
					{/each}
				{/each}
			</tbody>
		</table>
	{/if}
{/snippet}

{#snippet csvUpdateBatchRowErrors(result: BatchTagsUpdateResult, changedRows: CsvUpdateRow[])}
	{#if !result.ok}
		<table class="error-table">
			<thead>
				<tr>
					<th>行</th>
					<th>ID</th>
					<th>項目</th>
					<th>内容</th>
				</tr>
			</thead>
			<tbody>
				{#each result.errors as rowError (rowError.index)}
					{#each rowError.fieldErrors as fe, i (i)}
						<tr>
							<!--
								csvBatchRowErrors（新規追加用）と同じ理由で、ここでの
								`rowError.index` は `updateTagsBatch` に送った
								`csvUpdateClassification.updateRows`（= changed 行のみ）配列の
								添字。実際の CSV ファイル行番号に変換するには
								`changedRows[index].lineNumber` を引く必要がある
								（changedRows は classification.rows を category === 'changed' で
								絞った、updateRows と同じ並び順の配列 - `csvUpdateChangedRows`
								参照）。
							-->
							<td>{changedRows[rowError.index]?.lineNumber ?? `#${rowError.index}`}</td>
							<td>{rowError.id}</td>
							<td>{fe.field}</td>
							<td>{fe.message}</td>
						</tr>
					{/each}
				{/each}
			</tbody>
		</table>
	{/if}
{/snippet}

{#snippet previewLimitNote(totalCount: number)}
	<!--
		T18-5a（docs/banto-hub-t18-design.md「T18-5a 大量タグ性能」第1段）:
		連続登録/CSV新規/CSV更新差分/一括操作差分の4プレビュー表で共通の
		「表示だけ先頭 PREVIEW_DISPLAY_LIMIT 件に絞る」注記。検証・適用・
		件数サマリ・エラー一覧（batchRowErrors 系）はこの注記と無関係に
		全件を対象にしたまま動く。
	-->
	{#if totalCount > PREVIEW_DISPLAY_LIMIT}
		<p class="note">
			ほか {totalCount - PREVIEW_DISPLAY_LIMIT} 件は表示を省略しています（検証・適用は全 {totalCount}
			件を対象に行われます。エラーは下の一覧に全件表示されます）
		</p>
	{/if}
{/snippet}

<div class="page">
	<div class="page-header">
		<h2>タグ登録</h2>
	</div>

	<div class="content">
		<SplitPane leftWidth="280px">
			{#snippet left()}
				<div class="tree-pane">
					<!--
						T19 S1-a（docs/banto-hub-t19-design.md §7.1「常時表示の
						『新規作成』入口」）: 接続・グループの作成は右クリック
						（と Shift+F10）だけが入口だったため、旧 `plc-connections`/
						`collection-groups` 画面が持っていた常設ボタンに相当する
						入口をツリー上部に置く。タグ用ツールバー（右ペインの
						「新規登録」「連続登録」「CSVインポート」）とは対象が違う
						ため混ぜない - 別のツールバーとしてツリー側に置く。
						`canWrite` が無ければ出さない（既存の権限判定は緩めない）。
					-->
					{#if canWrite}
						<div class="tree-toolbar">
							<button type="button" class="secondary" onclick={openConnectionCreateDrawer}>
								PLC接続を追加
							</button>
							<button type="button" class="secondary" onclick={() => openGroupCreateDrawer()}>
								収集グループを追加
							</button>
						</div>
					{/if}
					<ConnectionTree
						{connections}
						{groups}
						{tags}
						selectedId={treeSelectedId}
						onselect={handleTreeSelect}
						oncontextmenu={handleTreeContextMenu}
					/>
				</div>
			{/snippet}
			{#snippet right()}
				<div class="right-pane">
					<div class="toolbar">
						{#if canWrite}
							<button type="button" onclick={openCreateDrawer}>新規登録</button>
							<button type="button" onclick={openContinuousDrawer}>連続登録</button>
							<button type="button" onclick={openCsvDrawer}>CSVインポート</button>
							<!-- T18-3b: 選択列を追加する代わりに、行クリックの意味そのものを
								「編集を開く」⇔「選択を切り替える」で切り替えるトグル
								（`selectTag`/`toggleSelectRow` の doc comment 参照）。 -->
							<button
								type="button"
								class="secondary"
								data-testid="tag-selection-mode-toggle"
								onclick={toggleSelectionMode}
							>
								{selectionMode ? '複数選択を終了' : '複数選択'}
							</button>
							<!-- T18-3e: セル編集/TSV貼付の表編集モード。収集停止中のみON にできる
								（停止中ロック、`toggleGridEditMode` の doc comment 参照）。ON中は
								selectionMode と相互排他。 -->
							<button
								type="button"
								class="secondary"
								data-testid="tag-grid-edit-mode-toggle"
								disabled={!gridEditMode && !collectionStopped}
								title={!collectionStopped ? '収集停止中のみ表編集できます' : undefined}
								onclick={toggleGridEditMode}
							>
								{gridEditMode ? '表編集を終了' : '表編集'}
							</button>
						{/if}
						<!-- T18-3d: 出力範囲（全件/絞り込み結果/選択行）。選択行は
							選択が1件も無ければ選べない（disabled option）。 -->
						<select
							class="csv-export-scope"
							data-testid="tag-csv-export-scope"
							bind:value={csvExportScope}
							title="CSVエクスポートの対象範囲"
						>
							<option value="all">全件（{tags.length}件）</option>
							<option value="filtered">絞り込み結果（{filteredTags.length}件）</option>
							<option value="selected" disabled={selectedIds.size === 0}>
								選択行（{selectedIds.size}件）
							</option>
						</select>
						<button type="button" class="secondary" onclick={handleExportCsv}
							>CSVエクスポート</button
						>
						<input
							type="search"
							class="search-box"
							placeholder="名前・アドレスで検索"
							bind:value={searchQuery}
						/>
						<span class="count">{filteredTags.length} / {tags.length} 件</span>
					</div>
					{#if canWrite && hubStatus !== null && !collectionStopped}
						<!-- T18-3e: 停止中ロックの説明バナー - `hubStatus` 取得前
							（`null`）は誤って「稼働中」と表示しないよう、取得済みの
							ときだけ出す（実装指示「稼働中は編集不可で…バナー/無効表示」）。 -->
						<p class="note" data-testid="tag-grid-edit-locked-note">
							収集稼働中のため表編集はできません（収集を停止すると表編集を有効にできます）。
						</p>
					{/if}
					{#if canWrite && gridEditMode && pendingCellEdits.length > 0}
						<!-- T18-3e: 保留中のセル編集バー（`tag-bulk-bar` と同じ帯パターン）。
							「保存」は preflight（dry-run）を挟んでから確認パネルを開く
							（`handleSaveGridEdits`）。件数は「実際に値が変わる行数」
							（`cellEditBatch.diffRows`）- 元に戻した編集は保留バッファには
							残るが、この件数には含めない。 -->
						<div class="onboarding-banner" data-testid="tag-cell-edit-bar">
							<span>保留中の編集 {cellEditBatch.diffRows.length} 件</span>
							<button
								type="button"
								data-testid="tag-cell-edit-save"
								disabled={cellEditValidating || cellEditBatch.rows.length === 0}
								onclick={handleSaveGridEdits}
							>
								{cellEditValidating ? '確認中…' : '保存'}
							</button>
							<button
								type="button"
								class="secondary"
								data-testid="tag-cell-edit-discard"
								disabled={cellEditValidating || cellEditApplying}
								onclick={discardGridEdits}
							>
								破棄
							</button>
						</div>
					{/if}
					{#if canWrite && cellEditPanelOpen}
						<!-- T18-3e: 保存前の差分確認パネル（`tag-bulk-confirm-panel` と同じ
							`.confirm-panel`/`.preview-table` を流用）。preflight
							（`dryRun: true`）の結果はエラー行表示にのみ使い、差分自体は
							クライアント側の `cellEditBatch.diffRows`（`tagCellEdit.ts`）を
							表示する。 -->
						<div class="confirm-panel" data-testid="tag-cell-edit-confirm-panel">
							<p class="confirm-title">表編集の保存内容を確認</p>
							<p class="note">対象 {cellEditBatch.diffRows.length} 件</p>
							<div class="preview-wrap">
								<table class="preview-table">
									<thead>
										<tr>
											<th>ID</th>
											<th>名前</th>
											<th>変更内容</th>
										</tr>
									</thead>
									<tbody>
										{#each cellEditBatch.diffRows.slice(0, PREVIEW_DISPLAY_LIMIT) as row (row.id)}
											<tr>
												<td>{row.id}</td>
												<td>{row.name}</td>
												<td>{formatCellEditDiffs(row.diffs)}</td>
											</tr>
										{/each}
									</tbody>
								</table>
							</div>
							{@render previewLimitNote(cellEditBatch.diffRows.length)}
							{#if cellEditValidationResult}
								{@render bulkRowErrors(cellEditValidationResult)}
							{/if}
							<div class="actions">
								<button
									type="button"
									data-testid="tag-cell-edit-apply"
									disabled={!cellEditValidatedFresh || cellEditApplying}
									onclick={handleApplyGridEdits}>この内容で保存を適用</button
								>
								<button
									type="button"
									class="secondary"
									data-testid="tag-cell-edit-cancel-confirm"
									onclick={cancelCellEditConfirm}
									disabled={cellEditApplying}>閉じる</button
								>
							</div>
						</div>
					{/if}
					{#if canWrite && selectedIds.size > 0}
						<!-- T18-3b: 選択が1件以上のときだけ出す一括操作バー
							（`monitorCtaHref` バナーと同じ帯パターン）。文言は既存トースト/
							ボタン名と部分文字列でも被らないものにしてある（PR #135 の教訓、
							`handleApplyBulk`/成功トーストのコメント参照）。 -->
						<div class="onboarding-banner" data-testid="tag-bulk-bar">
							<span>選択 {selectedIds.size} 件</span>
							<button
								type="button"
								data-testid="tag-bulk-enable-open"
								onclick={() => openBulkPanel('enable')}>一括で有効化</button
							>
							<button
								type="button"
								class="secondary"
								data-testid="tag-bulk-disable-open"
								onclick={() => openBulkPanel('disable')}>一括で無効化</button
							>
							<button
								type="button"
								class="secondary"
								data-testid="tag-bulk-move-open"
								disabled={selectedTagsMixedKind}
								title={selectedTagsMixedKind
									? '種別（plc/computed/internal）が混在する選択ではグループ移動できません'
									: undefined}
								onclick={() => openBulkPanel('move')}>グループへ一括移動</button
							>
							<button
								type="button"
								class="secondary"
								data-testid="tag-bulk-clear-selection"
								onclick={() => (selectedIds = new Set())}>選択解除</button
							>
						</div>
					{/if}
					{#if canWrite && bulkAction !== null}
						{@const actionLabel =
							bulkAction === 'enable'
								? '選択タグを一括で有効化'
								: bulkAction === 'disable'
									? '選択タグを一括で無効化'
									: '選択タグをグループへ一括移動'}
						<!-- T18-3b: 適用前に対象件数・差分を確認するパネル。連続登録/CSVの
							`.preview-table`/`.confirm-panel` をそのまま流用する（既存
							プレビュー表示との視覚的な一貫性を優先し、新規スタイルは足さない）。 -->
						<div class="confirm-panel" data-testid="tag-bulk-confirm-panel">
							<p class="confirm-title">{actionLabel}</p>
							{#if bulkAction === 'move'}
								<label class="field">
									移動先グループ
									<select bind:value={bulkTargetGroupId} data-testid="tag-bulk-target-group">
										<option value="" disabled>選択してください</option>
										{#each bulkMoveGroupOptions as group (group.id)}
											<option value={String(group.id)}>{group.name}</option>
										{/each}
									</select>
								</label>
							{/if}
							{#if bulkSummary}
								<p class="note">
									対象 {bulkSummary.targetCount} 件・変更 {bulkSummary.changedCount} 件
								</p>
								<div class="preview-wrap">
									<table class="preview-table">
										<thead>
											<tr>
												<th>ID</th>
												<th>名前</th>
												<th>変更前</th>
												<th>変更後</th>
											</tr>
										</thead>
										<tbody>
											{#each bulkSummary.rows.slice(0, PREVIEW_DISPLAY_LIMIT) as row (row.id)}
												<tr>
													<td>{row.id}</td>
													<td>{row.name}</td>
													<td>{bulkFieldDisplay(bulkAction, row.from)}</td>
													<td>{bulkFieldDisplay(bulkAction, row.to)}</td>
												</tr>
											{/each}
										</tbody>
									</table>
								</div>
								{@render previewLimitNote(bulkSummary.rows.length)}
							{:else if bulkAction === 'move'}
								<p class="hint">移動先グループを選択してください。</p>
							{/if}
							{#if bulkResult}
								{@render bulkRowErrors(bulkResult)}
							{/if}
							<div class="actions">
								<button
									type="button"
									data-testid="tag-bulk-apply"
									disabled={bulkApplying || bulkRows.length === 0}
									onclick={handleApplyBulk}>この内容で一括反映</button
								>
								<button
									type="button"
									class="secondary"
									data-testid="tag-bulk-cancel"
									onclick={closeBulkPanel}
									disabled={bulkApplying}>キャンセル</button
								>
							</div>
						</div>
					{/if}
					{#if monitorCtaHref}
						<!-- T18-4c（docs/banto-hub-t18-design.md「T18-4c 確認導線」）: 新規/
							複製/編集/連続登録/CSV取り込み/一括更新のいずれの成功後にも、
							サイドバー探索なしでその対象タグの値・品質・時刻へ1クリックで
							移動できるよう案内する。文言はどの成功トースト（`作成しました`
							`更新しました`/`削除しました`/一括登録・一括更新の件数付き文言）
							とも部分一致しないようにしてある - `page.getByText('作成しました')`
							等（`e2e/tests-banto-hub/banto-hub-tags-form.spec.ts`/
							`banto-hub-tags-p0-2-preflight.spec.ts`）がトーストと二重ヒットして
							strict mode violation になっていた実測回帰（2026-08-12、PR #135
							CI）の再発防止。 -->
						<div class="onboarding-banner">
							<span>モニタで値・品質・時刻を確認できます。</span>
							<a class="onboarding-cta" href={monitorCtaHref}>確認: 値・品質・時刻を見る</a>
							<button type="button" class="secondary" onclick={() => (monitorCtaHref = null)}
								>閉じる</button
							>
						</div>
					{/if}
					<p class="note">
						{#if !canWrite}
							閲覧のみ（編集には編集者以上の権限が必要です）。
						{:else if gridEditMode}
							セルをダブルクリックまたは選択して直接編集できます（Excel等からの貼り付けにも対応）。行を開くにはダブルクリックしてください。「保存」を押すまで反映されません。
						{:else if selectionMode}
							行をクリックすると選択の切り替えになります（編集は「複数選択を終了」してから）。
						{:else}
							行をクリックすると編集パネルが開きます。
						{/if}
					</p>
					<!--
						T18-1（TAG-UX-C 6点目、docs/banto-hub-desktop-plan.md §9.4）:
						初期読込中・初期読込失敗・再読込中(stale)・再読込失敗(stale)・
						真の空・検索/ツリーフィルタ0件・通常表示を区別する。
						「通信失敗をタグ0件と表示しない」ため、`loadError` が立って
						いる間は空のBantoGridも「タグがありません」も出さない
						（stale があれば一覧の上にバナー、無ければ再試行のみ）。
					-->
					{#if loading && tags.length === 0 && !loadError}
						<p class="loading">読み込み中…</p>
					{:else if loadError && tags.length === 0}
						<div class="empty-state">
							<p class="err">{loadError}</p>
							<button type="button" onclick={() => void reload()} disabled={loading}>
								{loading ? '再試行中…' : '再試行'}
							</button>
						</div>
					{:else}
						{#if loading}
							<p class="loading">再読込中…</p>
						{:else if loadError}
							<div class="reload-banner">
								<span class="err">{loadError}</span>
								<button type="button" class="secondary" onclick={() => void reload()}>再試行</button
								>
								<span class="hint">前回の読込内容を表示しています。</span>
							</div>
						{/if}
						{#if tags.length === 0}
							<!--
								T18-2d（TAG-UX-A「空状態を…不足する前工程と移動ボタンを示す」）:
								連鎖する前工程（PLC接続→収集グループ）のうち欠けているものを
								案内する。connections/groups が両方揃っていれば通常の空表示。
							-->
							<div class="empty-state">
								{#if connections.length === 0}
									<p class="note">先に PLC接続 を作成してください。</p>
									<a class="onboarding-cta" href="/plc-connections">PLC接続ページへ移動</a>
								{:else if groups.length === 0}
									<p class="note">先に 収集グループ を作成してください。</p>
									<a class="onboarding-cta" href="/collection-groups">収集グループページへ移動</a>
								{:else}
									<p class="note">タグがありません。上の「新規登録」から追加してください。</p>
								{/if}
							</div>
						{:else if filteredTags.length === 0}
							<p class="note">条件に一致するタグがありません。</p>
						{:else}
							<div class="grid-wrap">
								<!--
									T18-3e: `@banto/grid-svelte` の `GridState`
									（`node_modules/@banto/grid-svelte/src/state.svelte.ts`）は
									コンストラクタで受け取った `columns` を private フィールドへ
									一度だけ固定し（`this.order` も初期化時の列 id 一覧のまま）、
									以後 `columns` prop が差し替わっても再構築しない
									（`BantoGrid.svelte` の `const gridState = externalState ??
									new GridState(columns, ...)` は素の `const` - プロパティ変更で
									再実行されるリアクティブな式ではない）。そのため
									`gridEditMode` の変化で `columns` の中身（`editable`/
									`unit`・`decimals` 列の有無）を変えても、BantoGrid 側は
									初回マウント時の列定義のまま更新されない。
									`{#key gridEditMode}` でモード切替のたびに BantoGrid
									自体を作り直し（アンマウント→再マウント）、新しい
									`columns` で `GridState` を最初から組み立て直させることで
									回避する - grid-svelte 本体は変更しない制約の中で確実に
									列定義を反映させる標準的な Svelte パターン。表編集モードの
									ON/OFF は頻繁な操作ではないため、切替時に列幅ドラッグ・
									ソート・フィルタ・スクロール位置がリセットされるのは
									許容できるコストと判断した。
								-->
								{#key gridEditMode}
									<BantoGrid
										rows={gridDisplayRows}
										{columns}
										getRowId={(t) => t.id}
										onRowClick={canWrite
											? selectionMode
												? toggleSelectRow
												: selectTag
											: undefined}
										rowClass={tagRowClass}
										onCellEdit={canWrite && gridEditMode ? handleGridCellEdit : undefined}
										onRangePaste={canWrite && gridEditMode ? handleGridRangePaste : undefined}
									/>
								{/key}
							</div>
						{/if}
					{/if}
				</div>
			{/snippet}
		</SplitPane>
	</div>
</div>

{#if treeContextMenu}
	<TreeContextMenu
		x={treeContextMenu.x}
		y={treeContextMenu.y}
		items={treeContextMenu.items.map((action) => ({
			id: action.kind,
			label: action.label,
			onSelect: () => activateTreeContextMenuAction(action)
		}))}
		onClose={closeTreeContextMenu}
	/>
{/if}

<!--
	T18-6d: 接続/収集グループの管理 Drawer。単独ページ（`/plc-connections`/
	`/collection-groups`）と全く同じ部品・同じ渡し方 - このページの右クリック
	メニューから開く以外の違いはない。
-->
<ConnectionDrawer
	open={connectionDrawerOpen}
	connection={connectionDrawerTarget}
	existingNames={connections.map((c) => c.name)}
	requestDelete={connectionDrawerRequestDelete}
	readOnly={connectionDrawerReadOnly}
	onClose={closeConnectionDrawer}
	onSaved={handleConnectionDrawerSaved}
	onDeleted={handleConnectionDrawerDeleted}
/>

<CollectionGroupDrawer
	open={groupDrawerOpen}
	group={groupDrawerTarget}
	existingNames={groups.map((g) => g.name)}
	{connections}
	presetPlcConnectionId={groupDrawerPresetConnectionId}
	requestDelete={groupDrawerRequestDelete}
	readOnly={groupDrawerReadOnly}
	onClose={closeGroupDrawer}
	onSaved={handleGroupDrawerSaved}
	onDeleted={handleGroupDrawerDeleted}
/>

<!--
	T19 S1-b（UX-31、docs/banto-hub-t19-design.md §3.2「作成は前後関係を
	必要としない一方向の作業なので中央モーダルで集中させる」）: タグの
	新規登録（`drawerMode === 'create'` - 複製もここに含む、上の
	`openDuplicateDrawer` コメント参照）だけを中央モーダル（`Modal.svelte`）
	へ切り出す。編集・連続登録・CSVインポートは引き続き右ペイン
	（`Drawer.svelte`）のまま - 一覧を見ながら直す/取り込む作業のため
	（同designの§3.2）。`onclose`/`onRequestClose` は既存の
	`closeDrawer`/`confirmDiscardIfNeeded` をそのまま共有する（破棄確認・
	busy 中クローズ抑止のロジックは変えない）。
-->
<Modal
	open={drawerMode === 'create'}
	title={drawerTitle}
	width="560px"
	onclose={closeDrawer}
	onRequestClose={confirmDiscardIfNeeded}
>
	{#if drawerMode === 'create' && canWrite}
		<form
			class="drawer-section"
			onsubmit={(e) => {
				e.preventDefault();
				// T18-2c: どちらのボタンが送信を起こしたかは
				// `SubmitEvent.submitter` から判定する - `undefined`/`null`
				// （submitter を返さない実装での Enter 実装送信等）の場合は
				// DOM 上で先に置いた「登録して次へ」（closeAfterSave=false）を
				// 既定にする。`handleCreate` 側のコメントも参照。
				const submitter = (e as SubmitEvent).submitter;
				void handleCreate(submitter?.id === 'create-register-close');
			}}
		>
			{#if duplicateSource}
				<!--
					T18-3a（docs/banto-hub-t18-design.md「T18-3a タグ複製」、
					TAG-UX-D 前半「保存前に複製元との差分と外部名を確認できる」）:
					複製元タグと複製後フォームのフィールド単位差分。既存の
					revision 競合パネル（`.conflict-panel`/`.preview-table`、下の
					`{:else if drawerMode === 'edit'}` 側参照）と同じマークアップを
					流用し、新規 CSS は追加しない - 列見出しだけ「あなたの入力/
					サーバー最新」ではなく「複製元/複製後」に読み替える。
				-->
				<div class="conflict-panel">
					<h4 class="conflict-title">複製元との差分</h4>
					<p class="note">複製元: {externalNameForTag(duplicateSource)}</p>
					{#if duplicateDiff && duplicateDiff.length === 0}
						<p class="note">複製元と同じ内容です（名前・アドレスも含め差分はまだありません）。</p>
					{:else if duplicateDiff}
						<table class="preview-table">
							<thead>
								<tr>
									<th>項目</th>
									<th>複製元</th>
									<th>複製後</th>
								</tr>
							</thead>
							<tbody>
								{#each duplicateDiff as f (f.key)}
									<tr>
										<td>{f.label}</td>
										<td>{f.local}</td>
										<td>{f.server}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					{/if}
				</div>
			{/if}
			{@render tagFields(
				createForm,
				createErrors,
				createDetailOpen,
				createAddressPreflight,
				() => {
					// 2026-09-01 オーナー要望: アドレス欄の入力に追従して名前欄を
					// プリフィルする（`createNameTouched` が false の間だけ、
					// `$lib/banto/tagNamePrefill.ts` 参照）。`scheduleAddressPreflight`
					// より先に行うことで、プリフィルで名前欄が埋まった直後の
					// 入力から preflight の実行条件（`form.name.trim() !== ''`、
					// 下の `scheduleAddressPreflight` 定義参照）を満たせるように
					// する。
					const nextName = nextTagNameOnAddressChange(
						createForm.tagKind === 'plc',
						createForm.address,
						createNameTouched
					);
					if (nextName !== null) createForm.name = nextName;
					scheduleAddressPreflight(createForm, 'create');
				},
				() => {
					// 名前欄をユーザーが直接編集した合図 - 以後はアドレス入力に
					// 追従させない（`createNameTouched` 宣言のコメント参照）。
					createNameTouched = true;
				},
				() => {
					// T19 S1-b（UX-34）: `writable` チェックボックスをユーザーが
					// 直接クリックした合図 - 以後は自動計算しない
					// （`createWritableTouched` 宣言のコメント参照）。
					createWritableTouched = true;
				}
			)}
			<div class="actions">
				<!--
					T18-2c（TAG-UX-2「作成後は『登録して次へ』と『登録して閉じる』を
					分け…」）: 「登録して次へ」を先に置き、Enter 押下時の既定
					送信ボタンにする（`handleCreate` のコメント参照）。
				-->
				<button
					type="submit"
					id="create-register-next"
					disabled={isDrawerBusy() || groups.length === 0}
				>
					登録して次へ
				</button>
				<button
					type="submit"
					id="create-register-close"
					class="secondary"
					disabled={isDrawerBusy() || groups.length === 0}
				>
					登録して閉じる
				</button>
			</div>
			{#if groups.length === 0}
				<p class="note">
					先に 収集グループ を1件以上登録してください。
					<a class="onboarding-cta" href="/collection-groups">収集グループページへ移動</a>
				</p>
			{/if}
		</form>
	{/if}
</Modal>

<Drawer
	open={drawerMode === 'edit' || drawerMode === 'continuous' || drawerMode === 'csv'}
	title={drawerTitle}
	width={drawerWidth}
	onclose={closeDrawer}
	onRequestClose={confirmDiscardIfNeeded}
>
	{#if drawerMode === 'edit' && selected && canWrite}
		<form
			class="drawer-section"
			onsubmit={(e) => {
				e.preventDefault();
				void saveEdit();
			}}
		>
			{#if editConflict}
				<!--
					T18-1（TAG-UX-C 4点目「差分表示 UI」、docs/banto-hub-desktop-plan.md
					§9.4）: revision 競合の差分パネル。フォーム上部に置き、
					「あなたの入力（editForm、下のフォームにも反映済み）」と
					「サーバー最新」をフィールド単位で並べる。差分が0件（内容は
					同じだが revision だけ進んだ稀ケース）でもパネル自体は出す。
				-->
				<div class="conflict-panel">
					<h4 class="conflict-title">他のクライアントが先に更新しています</h4>
					{#if editConflict.fields.length === 0}
						<p class="note">内容は同じですが revision が進んでいます。</p>
					{:else}
						<table class="preview-table">
							<thead>
								<tr>
									<th>項目</th>
									<th>あなたの入力</th>
									<th>サーバー最新</th>
								</tr>
							</thead>
							<tbody>
								{#each editConflict.fields as f (f.key)}
									<tr>
										<td>{f.label}</td>
										<td>{f.local}</td>
										<td>{f.server}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					{/if}
					<div class="actions">
						<button
							type="button"
							class="secondary"
							onclick={resolveConflictWithServer}
							disabled={isDrawerBusy()}
						>
							サーバー最新を採用
						</button>
						<button
							type="button"
							onclick={() => void resolveConflictWithLocal()}
							disabled={isDrawerBusy()}
						>
							自分の内容で再保存
						</button>
					</div>
				</div>
			{/if}
			{@render tagFields(
				editForm,
				editErrors,
				editDetailOpen,
				editAddressPreflight,
				() => scheduleAddressPreflight(editForm, 'edit'),
				// 2026-09-01: 名前空欄→アドレス自動プリフィルは対象外（既存タグの
				// 名前を空にするのは「消したい」意図かもしれないため - 実装指示
				// どおり編集フォームは対象外にする）。何もしない no-op を渡す。
				() => {},
				// T19 S1-b（UX-34）: 既定の自動適用は create Drawer 限定
				// （`createWritableTouched` 宣言のコメント参照）- 編集フォームに
				// 対応する touched 変数は無いため no-op を渡す。
				() => {}
			)}
			<div class="actions">
				<button type="submit" disabled={isDrawerBusy()}>保存</button>
				<!--
					T18-3a（docs/banto-hub-t18-design.md「T18-3a タグ複製」）: 起動口。
					文言は既存トースト「作成しました」/「更新しました」/「削除しました」や
					ボタン名「新規登録」「登録して次へ」「登録して閉じる」「保存」
					「削除」と部分文字列としても被らないものにする（`tagTreeContextMenu.ts`
					冒頭コメントの教訓、PR #135 CI 回帰と同じ配慮）。
				-->
				<button
					type="button"
					class="secondary"
					data-testid="tag-duplicate-button"
					onclick={() => selected && openDuplicateDrawer(selected)}
					disabled={isDrawerBusy()}
				>
					このタグを複製
				</button>
				<button type="button" class="danger" onclick={handleDelete} disabled={isDrawerBusy()}
					>削除</button
				>
			</div>
		</form>
	{:else if drawerMode === 'continuous' && canWrite}
		<div class="drawer-section">
			<p class="note">
				名前パターン（<code>{'{n}'}</code>が連番に置き換わります。例:
				<code>temp{'{n}'}</code> + 開始1 + 3点 → temp1, temp2,
				temp3）・開始アドレス・点数・共通設定から連続タグを一括生成します。アドレスの増分はデータ型から自動決定（i16/u16
				等のワード型は+1、i32/u32/f32 は+2、string は文字列長分）。<code>.N</code
				>（ビット位置）付きのアドレス（例: <code>D100.5</code>）はワード内 bit 連番になります（bit15
				の次は次ワードの bit0）。<code>X</code>/<code>Y</code>/<code>B</code>/<code>W</code>/<code
					>SB</code
				>/<code>SW</code>/<code>DX</code>/<code>DY</code> は16進デバイス番号として桁上がりを扱います。
			</p>
			<div class="form-grid">
				<label class="field">
					対象グループ
					<select bind:value={continuousForm.collectionGroupId}>
						<option value="" disabled>選択してください</option>
						{#each groupsFor('plc') as group (group.id)}
							<option value={String(group.id)}>{group.name}</option>
						{/each}
					</select>
				</label>
				<label class="field">
					名前パターン
					<input
						type="text"
						bind:value={continuousForm.namePattern}
						placeholder="D{'{n}'}"
						oninput={() => {
							// T19 S1-b（UX-35）: ユーザーが直接編集した合図 - 以後は
							// 開始アドレス入力に追従させない（上の
							// `continuousNamePatternTouched` 宣言のコメント参照）。
							continuousNamePatternTouched = true;
						}}
					/>
				</label>
				<label class="field">
					開始番号
					<input
						type="number"
						bind:value={continuousForm.startNumber}
						oninput={() => {
							continuousStartNumberTouched = true;
						}}
					/>
				</label>
				<label class="field">
					開始アドレス
					<input
						type="text"
						bind:value={continuousForm.startAddress}
						placeholder="D3000"
						oninput={() => {
							// T19 S1-b（UX-35「名前パターンの既定をデバイス名から導出・
							// 開始番号は入力不要」）: 名前パターン・開始番号のどちらも
							// ユーザーがまだ直接編集していなければ、開始アドレスから
							// 導出した値へ追従させる（`tagNamePrefill.ts` の
							// `nextTagNameOnAddressChange` と同じ touched 追跡方式）。
							const nextPattern = nextNamePatternOnAddressChange(
								continuousForm.startAddress,
								continuousNamePatternTouched
							);
							if (nextPattern !== null) continuousForm.namePattern = nextPattern;
							const nextStart = nextStartNumberOnAddressChange(
								continuousForm.startAddress,
								continuousStartNumberTouched
							);
							if (nextStart !== null) continuousForm.startNumber = nextStart;
						}}
					/>
				</label>
				<label class="field">
					点数
					<input
						type="number"
						min="1"
						max={MAX_CONTINUOUS_COUNT}
						bind:value={continuousForm.count}
					/>
				</label>
				{@render continuousCommonFields()}
			</div>

			{#if groups.length === 0}
				<p class="note">
					先に 収集グループ を1件以上登録してください。
					<a class="onboarding-cta" href="/collection-groups">収集グループページへ移動</a>
				</p>
			{/if}

			{#if continuousPreview && !continuousPreview.ok}
				<p class="err">{continuousPreview.error}</p>
			{:else if continuousPreview?.ok}
				<h4>プレビュー（{continuousPreview.rows.length}件）</h4>
				<div class="preview-wrap">
					<table class="preview-table">
						<thead>
							<tr>
								<th>#</th>
								<th>名前</th>
								<th>アドレス</th>
							</tr>
						</thead>
						<tbody>
							{#each continuousPreview.rows.slice(0, PREVIEW_DISPLAY_LIMIT) as row, i (i)}
								<tr>
									<td>{i + 1}</td>
									<td>{row.name}</td>
									<td>{row.address}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
				{@render previewLimitNote(continuousPreview.rows.length)}

				{#if validationResult}
					{@render batchRowErrors(validationResult)}
				{/if}

				<div class="actions">
					<button type="button" onclick={handleValidateContinuous} disabled={isDrawerBusy()}
						>検証</button
					>
					<button
						type="button"
						onclick={handleApplyContinuous}
						disabled={!continuousValidatedFresh || isDrawerBusy()}>登録</button
					>
					{#if !continuousValidatedFresh}
						<span class="hint"
							>先に「検証」を実行してください（フォームを変更すると再検証が必要）。</span
						>
					{/if}
				</div>
			{/if}
		</div>
	{:else if drawerMode === 'csv' && canWrite}
		<div class="drawer-section">
			<!-- T18-3d: 「新規追加」/「既存更新」モード切替。文言は既存トースト/
				ボタン名と部分一致しないものにしてある（PR #135 の教訓）。 -->
			<div class="field csv-mode-field">
				<span class="csv-mode-label">インポートモード</span>
				<div class="csv-mode-options">
					<label>
						<input
							type="radio"
							name="csv-mode"
							value="create"
							checked={csvMode === 'create'}
							data-testid="tag-csv-mode-create"
							onchange={() => handleCsvModeChange('create')}
							disabled={isDrawerBusy()}
						/>
						新規追加
					</label>
					<label>
						<input
							type="radio"
							name="csv-mode"
							value="update"
							checked={csvMode === 'update'}
							data-testid="tag-csv-mode-update"
							onchange={() => handleCsvModeChange('update')}
							disabled={isDrawerBusy()}
						/>
						既存更新
					</label>
				</div>
			</div>

			{#if csvMode === 'create'}
				<p class="note">
					CSVファイル（列名ヘッダ付き・タグ登録フォームの項目と1:1対応、接続・グループは名前で参照 —
					存在しない名前はエラーになります。自動作成はしません）をアップロードすると、
					内容を検証してからプレビュー表示します。連続登録と同じく「検証 → 登録」の2段階です。
					<strong>新規追加モードでは既存タグは更新されません</strong>。
					エクスポートしたCSVをそのまま再インポートすると、全行が名前重複エラーになります
					（想定どおりの挙動です）。
				</p>
			{:else}
				<p class="note">
					既存タグを CSV で一括更新します。突き合わせは「接続＋グループ＋名前」で行い、
					一致した行のうち<strong>実際に値が変わる行だけ</strong>を更新します。
					<strong>CSV に無い既存タグは変更されません</strong>（暗黙の削除はしません）。 CSV
					にはあるが既存に無い名前の行は追加登録されません（新規追加モードを使ってください）。
				</p>
			{/if}

			<div class="actions">
				<button
					type="button"
					class="secondary"
					data-testid="tag-csv-template-download"
					onclick={handleDownloadCsvTemplate}>テンプレートをダウンロード</button
				>
			</div>

			<div class="field">
				<input
					type="file"
					accept=".csv"
					bind:this={csvFileInputEl}
					onchange={handleCsvFileChange}
					disabled={isDrawerBusy()}
				/>
			</div>

			{#if csvSizeError}
				<p class="err" data-testid="tag-csv-size-error">{csvSizeError}</p>
			{:else if csvRowLimitError}
				<p class="err" data-testid="tag-csv-row-limit-error">{csvRowLimitError}</p>
			{:else if csvParseResult && !csvParseResult.ok}
				<h4>エラー（{csvParseResult.errors.length}件）</h4>
				<p class="note">ファイルを修正して再アップロードしてください。</p>
				<div class="preview-wrap">
					{@render csvParseErrors(csvParseResult.errors)}
				</div>
				<div class="actions">
					<button
						type="button"
						class="secondary"
						data-testid="tag-csv-error-download"
						onclick={handleDownloadCsvErrorRows}>エラー行をCSVでダウンロード</button
					>
				</div>
			{:else if csvParseResult?.ok && csvParseResult.rows.length === 0}
				<p class="note">インポートする行がありません。</p>
			{:else if csvParseResult?.ok && csvMode === 'create'}
				<h4>プレビュー（{csvParseResult.rows.length}件）</h4>
				<div class="preview-wrap">
					<table class="preview-table">
						<thead>
							<tr>
								<th>行</th>
								<th>接続</th>
								<th>グループ</th>
								<th>名前</th>
								<th>アドレス</th>
								<th>データ型</th>
								<th>種別</th>
								<th>有効</th>
								<th>書き込み可</th>
							</tr>
						</thead>
						<tbody>
							{#each csvParseResult.rows.slice(0, PREVIEW_DISPLAY_LIMIT) as row (row.lineNumber)}
								<tr>
									<td>{row.lineNumber}</td>
									<td>{row.connectionName}</td>
									<td>{row.groupName}</td>
									<td>{row.tag.name}</td>
									<td>{row.tag.address}</td>
									<td>{row.tag.dataType}</td>
									<td>{row.tag.tagKind ?? 'plc'}</td>
									<td>{row.tag.enabled ? 'はい' : 'いいえ'}</td>
									<td>{row.tag.writable ? 'はい' : 'いいえ'}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
				{@render previewLimitNote(csvParseResult.rows.length)}

				{#if csvValidationResult}
					{@render csvBatchRowErrors(csvValidationResult, csvParseResult.rows)}
				{/if}

				<div class="actions">
					<button type="button" onclick={handleValidateCsv} disabled={isDrawerBusy()}>検証</button>
					<button
						type="button"
						onclick={handleApplyCsv}
						disabled={!csvValidatedFresh || isDrawerBusy()}>登録</button
					>
					{#if !csvValidatedFresh}
						<span class="hint"
							>先に「検証」を実行してください（ファイルを差し替えると再検証が必要）。</span
						>
					{/if}
				</div>
			{:else if csvParseResult?.ok && csvMode === 'update' && csvUpdateClassification}
				<h4>差分プレビュー（{csvUpdateClassification.rows.length}件）</h4>
				<p class="note" data-testid="tag-csv-update-summary">
					追加 {csvUpdateClassification.addedCount} / 変更 {csvUpdateClassification.changedCount}
					/ 変更なし {csvUpdateClassification.unchangedCount} / エラー {csvUpdateClassification.errorCount}
					件
				</p>
				<div class="preview-wrap">
					<table class="preview-table">
						<thead>
							<tr>
								<th>行</th>
								<th>名前</th>
								<th>区分</th>
								<th>内容</th>
							</tr>
						</thead>
						<tbody>
							{#each csvUpdateClassification.rows.slice(0, PREVIEW_DISPLAY_LIMIT) as row (row.lineNumber)}
								<tr>
									<td>{row.lineNumber}</td>
									<td>{row.name}</td>
									<td>{csvUpdateCategoryLabel(row.category)}</td>
									<td>
										{#if row.category === 'changed'}
											{formatCsvUpdateDiffs(row.diffs)}
										{:else}
											{row.message ?? ''}
										{/if}
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
				{@render previewLimitNote(csvUpdateClassification.rows.length)}

				{#if csvUpdateClassification.errorCount > 0}
					<div class="actions">
						<button
							type="button"
							class="secondary"
							data-testid="tag-csv-error-download"
							onclick={handleDownloadCsvErrorRows}>エラー行をCSVでダウンロード</button
						>
					</div>
				{/if}

				{#if csvUpdateClassification.updateRows.length === 0}
					<p class="note">変更対象がありません。</p>
				{/if}

				{#if csvUpdateValidationResult}
					{@render csvUpdateBatchRowErrors(csvUpdateValidationResult, csvUpdateChangedRows)}
				{/if}

				<div class="actions">
					<button
						type="button"
						data-testid="tag-csv-update-validate"
						onclick={handleValidateCsvUpdate}
						disabled={csvUpdateClassification.updateRows.length === 0 || isDrawerBusy()}
						>検証</button
					>
					<button
						type="button"
						data-testid="tag-csv-update-apply"
						onclick={handleApplyCsvUpdate}
						disabled={!csvUpdateValidatedFresh || isDrawerBusy()}>更新を適用</button
					>
					{#if csvUpdateClassification.updateRows.length > 0 && !csvUpdateValidatedFresh}
						<span class="hint"
							>先に「検証」を実行してください（ファイルを差し替えると再検証が必要）。</span
						>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
</Drawer>

<style>
	.page {
		height: calc(100vh - var(--banto-shell-header-height) - 2.5rem);
		display: flex;
		flex-direction: column;
		min-height: 0;
		gap: 0.75rem;
	}

	.page-header {
		flex: 0 0 auto;
	}

	.page-header h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.content {
		flex: 1;
		min-height: 0;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		overflow: hidden;
	}

	/*
	 * T19 S1-a: ツリーペイン全体のラッパ。`SplitPane.svelte::.pane-left` が
	 * `overflow-y: auto` を持つため、ツリー本体と常設ボタンを縦積みにして
	 * スクロール領域を共有する（別スクロール領域には分けない - ツリーが
	 * 空/短い環境で不要な二重スクロールバーを避けるため）。
	 */
	.tree-pane {
		display: flex;
		flex-direction: column;
		min-height: 100%;
	}

	/* T19 S1-a: 接続・グループの常設作成ボタン。右ペインの `.toolbar`（タグ用）
	   とは対象が違うため混ぜない - 見た目もツリー直上の帯として区別する。 */
	.tree-toolbar {
		flex: 0 0 auto;
		display: flex;
		gap: 0.5rem;
		padding: 0.6rem 0.75rem;
		border-bottom: 1px solid var(--banto-border);
		position: sticky;
		top: 0;
		background: var(--banto-surface);
		z-index: 1;
	}

	.tree-toolbar button {
		font-size: 0.78rem;
		padding: 0.35rem 0.6rem;
	}

	.right-pane {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		gap: 0.6rem;
		padding: 1rem 1.25rem;
	}

	.toolbar {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 0.6rem;
		flex-wrap: wrap;
	}

	/* T18-3d: CSVエクスポートの出力範囲セレクタ（ツールバー内、既存の
		`.field select` は Drawer 内専用のため流用せずここで最小定義する）。 */
	.csv-export-scope {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-size: 0.8rem;
		font-family: inherit;
	}

	.search-box {
		margin-left: auto;
		min-width: 220px;
		padding: 0.4rem 0.6rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-size: 0.8rem;
		font-family: inherit;
	}

	.count {
		flex: 0 0 auto;
		color: var(--banto-text-muted);
		font-size: 0.75rem;
	}

	.note {
		flex: 0 0 auto;
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	/* T18-1（TAG-UX-C 6点目）: 初期読込失敗（一覧なし）時のエラー文言 + 再試行ボタン。 */
	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.5rem;
	}

	/* T18-2d（TAG-UX-A）: 前工程への移動リンク・作成直後の「次へ」導線バナー。 */
	.onboarding-cta {
		display: inline-block;
		padding: 0.3rem 0.75rem;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		font-size: 0.8rem;
		text-decoration: none;
		white-space: nowrap;
	}

	.onboarding-cta:hover {
		background: var(--banto-primary-hover);
	}

	.onboarding-banner {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.75rem;
		margin-bottom: 0.75rem;
		padding: 0.5rem 0.8rem;
		border: 1px solid var(--banto-primary);
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		font-size: 0.85rem;
	}

	/* 再読込失敗だが stale 一覧が残っている場合の、一覧上部のエラーバナー。 */
	.reload-banner {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 0.6rem;
		flex-wrap: wrap;
	}

	.grid-wrap {
		flex: 1;
		min-height: 0;
	}

	.drawer-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

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
	}

	.field.checkbox {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
	}

	.field.wide {
		grid-column: 1 / -1;
	}

	/* T19 S1-b（UX-36）: 連続登録フォームの詳細セクション2つを form-grid 内で全幅にする。 */
	.continuous-detail-wrap {
		grid-column: 1 / -1;
	}

	.field input,
	.field select,
	.field textarea {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-family: inherit;
	}

	.field textarea {
		resize: vertical;
	}

	.field.checkbox input {
		width: auto;
	}

	.hint {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	/*
	 * T19 S1-b（UX-34）: `writable` チェックボックスが無効化されている理由
	 * （`#tag-writable-blocked-reason`）を form-grid の全幅で表示する。
	 * `.warn` と同じ「独立して grid-column を持つ」流儀（`.wide` 単体クラス
	 * は `.field.wide` にしか効かないため）。
	 */
	.hint.wide {
		grid-column: 1 / -1;
		margin: 0;
	}

	.required {
		margin-left: 0.15rem;
		color: var(--banto-danger);
	}

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
	}

	/*
	 * T18-2b（TAG-UX-6「入力中に共通 preflight を実行する」）: アドレス欄の
	 * デバウンス dry-run 検証の参考表示。送信時の `.err`（固定でエラー色）
	 * と違い、確認中/OK/エラーの3状態を持つのでそれぞれ控えめな色分けに
	 * する。
	 */
	.address-preflight-checking {
		color: var(--banto-text-muted);
	}

	.address-preflight-ok {
		color: var(--banto-success, #1a7f37);
	}

	.address-preflight-error {
		color: var(--banto-danger);
	}

	/*
	 * T18-2a（TAG-UX-B「常用する基本設定と、詳細設定を fieldset /
	 * 折りたたみで分ける」）: 表示・スケーリング／しきい値／書き込み安全
	 * 設定の3つの `<details>`。基本 form-grid と視覚的に区切るため上に
	 * 罫線を引く。
	 */
	.detail-group {
		border-top: 1px solid var(--banto-border);
		padding-top: 0.6rem;
		margin-top: 0.1rem;
	}

	.detail-group summary {
		cursor: pointer;
		padding: 0.3rem 0;
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--banto-text);
	}

	.detail-group[open] summary {
		margin-bottom: 0.3rem;
	}

	/*
	 * T19 S1-b（UX-36「値が設定されているときは、閉じていてもそれが分かる
	 * ようにする」）: 詳細セクションが閉じたままでも視認できるバッジ。
	 * 色は warning ではなく primary 系（危険ではなく「情報あり」の意味）。
	 */
	.detail-value-badge {
		display: inline-block;
		margin-left: 0.5rem;
		padding: 0.05rem 0.5rem;
		border-radius: 999px;
		font-size: 0.7rem;
		font-weight: 600;
		color: var(--banto-primary);
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
		vertical-align: middle;
	}

	/* T18-2a（TAG-UX-B「書き込み許可…ON 時に安全上の影響を説明する」）。 */
	.warn {
		grid-column: 1 / -1;
		margin: 0;
		padding: 0.6rem 0.75rem;
		border: 1px solid var(--banto-warning);
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-warning) 12%, transparent);
		color: var(--banto-text);
		font-size: 0.78rem;
	}

	/*
	 * T18-2a（TAG-UX-B「保存前に…固定領域で確認できるようにする」）: 保存
	 * 前の確認領域。フォームの他部分と視覚的に区別できるよう枠付きの箱に
	 * する。
	 */
	.confirm-panel {
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface-raised);
		padding: 0.75rem;
		margin-bottom: 0.75rem;
	}

	.confirm-title {
		margin: 0 0 0.5rem;
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--banto-text);
	}

	.confirm-list {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		margin: 0;
	}

	.confirm-row {
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
	}

	.confirm-row dt {
		flex: 0 0 auto;
		min-width: 6.5rem;
		color: var(--banto-text-muted);
		font-size: 0.72rem;
	}

	.confirm-row dd {
		margin: 0;
		color: var(--banto-text);
		font-size: 0.8rem;
		font-weight: 600;
		word-break: break-all;
	}

	.actions {
		display: flex;
		gap: 0.75rem;
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

	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}

	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
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

	h4 {
		margin: 0.75rem 0 0.5rem;
		font-size: 0.85rem;
	}

	.preview-wrap {
		max-height: 260px;
		overflow: auto;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
	}

	/* T18-3d: CSV インポートモード切替（新規追加/既存更新）のラジオ行。 */
	.csv-mode-field {
		flex-direction: row;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.75rem;
	}

	.csv-mode-label {
		font-weight: 600;
	}

	.csv-mode-options {
		display: flex;
		gap: 1rem;
	}

	.csv-mode-options label {
		display: flex;
		align-items: center;
		gap: 0.3rem;
		font-weight: normal;
		color: var(--banto-text);
	}

	.csv-mode-options input {
		width: auto;
	}

	.preview-table,
	.error-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.8rem;
	}

	.preview-table th,
	.preview-table td,
	.error-table th,
	.error-table td {
		padding: 0.35rem 0.6rem;
		border-bottom: 1px solid var(--banto-border);
		text-align: left;
	}

	.error-table {
		margin-top: 0.5rem;
	}

	.error-table th,
	.error-table td {
		color: var(--banto-danger);
	}

	/* T18-1（TAG-UX-C 4点目「差分表示 UI」）: revision 競合時にフォーム上部へ出す差分パネル。 */
	.conflict-panel {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.75rem;
		border: 1px solid var(--banto-danger);
		border-radius: var(--banto-radius);
	}

	.conflict-title {
		margin: 0;
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--banto-danger);
	}

	/*
	 * T18-3b（一括操作）: 選択モード中に選択済みの行を一覧で一目で分かる
	 * よう強調する。plc-connections ページの `.sim-row`（spec M14 の
	 * rowClass 仕組み）と同じパターン - BantoGrid 内部の DOM はこの
	 * コンポーネントのスコープド CSS の外なので :global が必要。
	 */
	:global(.row.tag-row-selected) {
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
		border-left: 3px solid var(--banto-primary);
	}
</style>
