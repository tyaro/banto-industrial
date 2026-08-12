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
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import Drawer from '$lib/components/Drawer.svelte';
	import SplitPane from '$lib/components/SplitPane.svelte';
	import ConnectionTree from '$lib/components/ConnectionTree.svelte';
	import type { ConnectionTreeNodeData } from '$lib/components/connectionTreeTypes';
	import {
		listTags,
		createTag,
		updateTag,
		deleteTag,
		listCollectionGroups,
		listPlcConnections,
		createTagsBatch,
		isTagRevisionConflictError,
		MIN_STRING_LENGTH,
		MAX_STRING_LENGTH,
		TAG_KIND_OPTIONS,
		CALC_CONNECTION_NAME,
		MEM_CONNECTION_NAME,
		type Tag,
		type TagInput,
		type TagDataType,
		type TagKind,
		type CollectionGroup,
		type PlcConnection,
		type BatchTagsResult
	} from '$lib/banto/tagRegistryAdmin';
	import {
		buildContinuousParams,
		generateContinuousTags,
		MAX_CONTINUOUS_COUNT,
		type ContinuousFormState,
		type ContinuousRegistrationResult
	} from '$lib/banto/continuousRegistration';
	import {
		exportTagsCsv,
		parseTagsCsv,
		type ImportTagsCsvResult,
		type ParsedCsvTagRow,
		type CsvRowError
	} from '$lib/banto/tagCsv';
	import { parseOptionalNumber } from '$lib/banto/tagFormNumeric';
	import {
		DISPLAY_SCALING_FIELDS,
		THRESHOLD_FIELDS,
		WRITE_SAFETY_FIELDS,
		hasFieldError,
		buildConfirmExternalName,
		environmentLabel,
		writePermissionLabel,
		fieldErrorsFromList
	} from '$lib/banto/tagFormLayout';
	import { addressHelpFor } from '$lib/banto/tagAddressHelp';
	import { isFormDirty } from '$lib/banto/formDirty';
	import {
		buildExternalName,
		findReferencingComputedTags,
		formatDeleteConfirmMessage
	} from '$lib/banto/tagDeleteImpact';
	import { diffFormRecords, type ConflictFieldDiff } from '$lib/banto/tagConflictDiff';
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
		} catch (err) {
			loadError = errorMessage(err);
			toastStore.push('error', loadError);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
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

	async function handleCreate(): Promise<void> {
		creating = true;
		createErrors = {};
		try {
			await createTag(toInput(createForm));
			toastStore.push('success', '作成しました');
			createForm = blankForm();
			createBaseline = blankForm();
			createAddressPreflight = blankAddressPreflight();
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) createErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			creating = false;
		}
	}

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
			csvApplying
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

	function openCreateDrawer(): void {
		if (!confirmDiscardIfNeeded()) return;
		createForm = blankForm();
		createBaseline = blankForm();
		createErrors = {};
		createAddressPreflight = blankAddressPreflight();
		editConflict = null;
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
		// T18-2b: 保留中のデバウンス preflight があれば止める - 閉じた後の
		// Drawer に対して古い結果が届いても表示先が無いので無害だが、
		// 不要な `/api/tags/batch` 呼び出し自体を止めておく。
		if (addressPreflightTimer !== undefined) clearTimeout(addressPreflightTimer);
	}

	// T18-1: 画面遷移（サイドバーの他画面リンク等）でも Esc/× と同じ破棄
	// 確認を行う。`confirmDiscardIfNeeded` が `false` を返した（busy 中、
	// または dirty で確認をキャンセルされた）場合は遷移そのものを止める。
	beforeNavigate((nav) => {
		if (drawerMode !== null && !confirmDiscardIfNeeded()) nav.cancel();
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

	// --- T11-1: 連続登録 (docs/ux-plan.md §3) ------------------------------
	//
	// 名前パターン・開始番号・開始アドレス・点数・共通設定から
	// `generateContinuousTags`（純関数、$lib/banto/continuousRegistration.ts）
	// でプレビュー行を組み立て、確認後に一括 API を叩く。連続登録は PLC
	// アドレスを前提とする機能のため tagKind は常に 'plc'（TagInput 側の
	// 既定と同じ、フォーム自体に種別選択は出さない）。

	function blankContinuousForm(): ContinuousFormState {
		return {
			collectionGroupId: '',
			namePattern: 'temp{n}',
			startNumber: '1',
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
				continuousForm = blankContinuousForm();
				continuousBaseline = blankContinuousForm();
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

	// --- T11-2: CSV エクスポート/インポート (docs/ux-plan.md §3) -------------
	//
	// エクスポートはこのページが Blob/DOM 操作を担当し（`$lib/banto/tagCsv.ts`
	// はブラウザ API に依存しない純関数のまま保つ）、インポートは連続登録と
	// 同じ「プレビュー → 検証(dry-run) → 登録」の2段階フローを踏襲する。

	/** ローカル日付での `banto-hub-tags-YYYY-MM-DD.csv`（設計: ux-plan.md §3）。 */
	function csvExportFilename(): string {
		const now = new Date();
		const y = now.getFullYear();
		const m = String(now.getMonth() + 1).padStart(2, '0');
		const d = String(now.getDate()).padStart(2, '0');
		return `banto-hub-tags-${y}-${m}-${d}.csv`;
	}

	/**
	 * 閲覧者でも実行可（`canWrite` でガードしない — 設定のバックアップ/
	 * レビューは読み取り専用の操作のため）。BOM は `exportTagsCsv` が
	 * 既に埋め込み済みなのでここで二重に付けない。
	 */
	function handleExportCsv(): void {
		const csv = exportTagsCsv(tags, connections, groups);
		const blob = new Blob([csv], { type: 'text/csv;charset=utf-8' });
		const url = URL.createObjectURL(blob);
		const a = document.createElement('a');
		a.href = url;
		a.download = csvExportFilename();
		a.click();
		URL.revokeObjectURL(url);
	}

	let csvFileInputEl: HTMLInputElement | undefined = $state();
	let csvParseResult: ImportTagsCsvResult | null = $state(null);

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

	// 連続登録と同じ鮮度追跡 — 検証後にファイルを差し替えたら「登録」を
	// 無効化し、再検証を要求する。
	let csvValidatedTagsJson = $state<string | null>(null);
	let csvValidationResult = $state<BatchTagsResult | null>(null);
	let csvValidating = $state(false);
	let csvApplying = $state(false);

	const csvValidatedFresh = $derived(
		csvTagsJson !== null && csvTagsJson === csvValidatedTagsJson && csvValidationResult?.ok === true
	);

	function invalidateCsvValidation(): void {
		csvValidatedTagsJson = null;
		csvValidationResult = null;
	}

	function resetCsvImport(): void {
		csvParseResult = null;
		invalidateCsvValidation();
		if (csvFileInputEl) csvFileInputEl.value = '';
	}

	async function handleCsvFileChange(e: Event): Promise<void> {
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		const text = await file.text();
		csvParseResult = parseTagsCsv(text, connections, groups);
		invalidateCsvValidation();
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

	const columns: GridColumn<Tag>[] = [
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
			format: (v) => (v ? 'はい' : 'いいえ')
		},
		{
			id: 'writable',
			header: '書き込み可',
			accessor: 'writable',
			width: 90,
			format: (v) => (v ? 'はい' : 'いいえ')
		}
	];
</script>

{#snippet tagFields(
	form: FormState,
	errors: Record<string, string>,
	detailOpen: DetailOpenState,
	addressPreflight: AddressPreflightState,
	onAddressInput: () => void
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
		<summary>表示・スケーリング</summary>
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
		<summary>しきい値</summary>
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
		<details class="detail-group" bind:open={detailOpen.write}>
			<summary>書き込み安全設定</summary>
			<div class="form-grid">
				<label class="field checkbox wide">
					<input id="tag-writable" type="checkbox" bind:checked={form.writable} />
					外部クライアントから PLC への書き込みを許可
				</label>
				{#if form.writable}
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
	<label class="field checkbox">
		<input type="checkbox" bind:checked={continuousForm.enabled} />
		有効
	</label>
	<label class="field checkbox">
		<input type="checkbox" bind:checked={continuousForm.writable} />
		書き込み可（writable）
	</label>
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

<div class="page">
	<div class="page-header">
		<h2>タグ登録</h2>
	</div>

	<div class="content">
		<SplitPane leftWidth="280px">
			{#snippet left()}
				<ConnectionTree
					{connections}
					{groups}
					{tags}
					selectedId={treeSelectedId}
					onselect={handleTreeSelect}
				/>
			{/snippet}
			{#snippet right()}
				<div class="right-pane">
					<div class="toolbar">
						{#if canWrite}
							<button type="button" onclick={openCreateDrawer}>新規登録</button>
							<button type="button" onclick={openContinuousDrawer}>連続登録</button>
							<button type="button" onclick={openCsvDrawer}>CSVインポート</button>
						{/if}
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
					<p class="note">
						{canWrite
							? '行をクリックすると編集パネルが開きます。'
							: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
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
							<p class="note">タグがありません。</p>
						{:else if filteredTags.length === 0}
							<p class="note">条件に一致するタグがありません。</p>
						{:else}
							<div class="grid-wrap">
								<BantoGrid
									rows={filteredTags}
									{columns}
									getRowId={(t) => t.id}
									onRowClick={canWrite ? selectTag : undefined}
								/>
							</div>
						{/if}
					{/if}
				</div>
			{/snippet}
		</SplitPane>
	</div>
</div>

<Drawer
	open={drawerMode !== null}
	title={drawerTitle}
	width={drawerWidth}
	onclose={closeDrawer}
	onRequestClose={confirmDiscardIfNeeded}
>
	{#if drawerMode === 'create' && canWrite}
		<form
			class="drawer-section"
			onsubmit={(e) => {
				e.preventDefault();
				void handleCreate();
			}}
		>
			{@render tagFields(createForm, createErrors, createDetailOpen, createAddressPreflight, () =>
				scheduleAddressPreflight(createForm, 'create')
			)}
			<div class="actions">
				<button type="submit" disabled={isDrawerBusy() || groups.length === 0}>作成</button>
			</div>
			{#if groups.length === 0}
				<p class="note">先に 収集グループ を1件以上登録してください。</p>
			{/if}
		</form>
	{:else if drawerMode === 'edit' && selected && canWrite}
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
			{@render tagFields(editForm, editErrors, editDetailOpen, editAddressPreflight, () =>
				scheduleAddressPreflight(editForm, 'edit')
			)}
			<div class="actions">
				<button type="submit" disabled={isDrawerBusy()}>保存</button>
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
				等のワード型は+1、i32/u32/f32 は+2、string は文字列長分）。ビット指定アドレス（<code
					>D100.5</code
				>のような形式）や、16進数値デバイス（<code>X</code>/<code>Y</code>/<code>B</code>/<code
					>W</code
				>/<code>SB</code>/<code>SW</code>/<code>DX</code>/<code>DY</code
				>）の連続登録は現時点では未対応です。
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
					<input type="text" bind:value={continuousForm.namePattern} placeholder="temp{'{n}'}" />
				</label>
				<label class="field">
					開始番号
					<input type="number" bind:value={continuousForm.startNumber} />
				</label>
				<label class="field">
					開始アドレス
					<input type="text" bind:value={continuousForm.startAddress} placeholder="D3000" />
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
				<p class="note">先に 収集グループ を1件以上登録してください。</p>
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
							{#each continuousPreview.rows as row, i (i)}
								<tr>
									<td>{i + 1}</td>
									<td>{row.name}</td>
									<td>{row.address}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>

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
			<p class="note">
				CSVファイル（列名ヘッダ付き・タグ登録フォームの項目と1:1対応、接続・グループは名前で参照 —
				存在しない名前はエラーになります。自動作成はしません）をアップロードすると、
				内容を検証してからプレビュー表示します。連続登録と同じく「検証 → 登録」の2段階で、
				<strong>新規登録専用</strong>です（既存タグの更新/upsert には対応していません）。
				エクスポートしたCSVをそのまま再インポートすると、全行が名前重複エラーになります
				（想定どおりの挙動です）。
			</p>
			<div class="field">
				<input
					type="file"
					accept=".csv"
					bind:this={csvFileInputEl}
					onchange={handleCsvFileChange}
					disabled={isDrawerBusy()}
				/>
			</div>

			{#if csvParseResult && !csvParseResult.ok}
				<h4>エラー（{csvParseResult.errors.length}件）</h4>
				<p class="note">ファイルを修正して再アップロードしてください。</p>
				<div class="preview-wrap">
					{@render csvParseErrors(csvParseResult.errors)}
				</div>
			{:else if csvParseResult?.ok && csvParseResult.rows.length === 0}
				<p class="note">インポートする行がありません。</p>
			{:else if csvParseResult?.ok}
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
							{#each csvParseResult.rows as row (row.lineNumber)}
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
</style>
