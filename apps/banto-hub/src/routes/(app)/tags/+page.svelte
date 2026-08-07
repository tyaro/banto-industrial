<script lang="ts">
	/**
	 * タグ登録（tags）CRUD 画面。plc-connections/collection-groups と同じ
	 * シンプル型を反復した新規作成（実装指示: 「tags 画面は 1737 行版
	 * （一括/連続登録込み）をコピーしない」）。
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
	import {
		listTags,
		createTag,
		updateTag,
		deleteTag,
		listCollectionGroups,
		listPlcConnections,
		createTagsBatch,
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
		generateContinuousTags,
		MAX_CONTINUOUS_COUNT,
		type ContinuousRegistrationParams,
		type ContinuousRegistrationResult
	} from '$lib/banto/continuousRegistration';
	import {
		exportTagsCsv,
		parseTagsCsv,
		type ImportTagsCsvResult,
		type ParsedCsvTagRow,
		type CsvRowError
	} from '$lib/banto/tagCsv';

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

	/** 空文字列 = 未設定（省略してバックエンドの `#[serde(default)]` に None として扱わせる）。 */
	function optNum(s: string): number | undefined {
		return s === '' ? undefined : Number(s);
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
			stringLength: form.dataType === 'string' ? optNum(form.stringLength) : undefined,
			rawLo: optNum(form.rawLo),
			rawHi: optNum(form.rawHi),
			engLo: optNum(form.engLo),
			engHi: optNum(form.engHi),
			unit: form.unit === '' ? undefined : form.unit,
			decimals: Number(form.decimals),
			thresholdH: optNum(form.thresholdH),
			thresholdHh: optNum(form.thresholdHh),
			thresholdL: optNum(form.thresholdL),
			thresholdLl: optNum(form.thresholdLl),
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
	 * T11-1/T11-2 (docs/ux-plan.md §3): 通常の単発登録フォーム / 連続登録
	 * フォーム / CSV インポートの切替。
	 */
	type Mode = 'single' | 'continuous' | 'csv';
	let mode: Mode = $state('single');

	let groups: CollectionGroup[] = $state([]);
	let connections: PlcConnection[] = $state([]);
	let tags: Tag[] = $state([]);
	let loading = $state(false);

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

	async function reload(): Promise<void> {
		loading = true;
		try {
			[groups, connections, tags] = await Promise.all([
				listCollectionGroups(),
				listPlcConnections(),
				listTags()
			]);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	// --- create ---
	let createForm = $state(blankForm());
	let createErrors: Record<string, string> = $state({});
	let creating = $state(false);

	function applyFieldErrors(err: unknown): Record<string, string> | null {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const map: Record<string, string> = {};
			for (const fe of err.body.field_errors) map[fe.field] = fe.message;
			return map;
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
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);

	function selectTag(t: Tag): void {
		selected = t;
		editForm = formFromTag(t);
		editErrors = {};
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updateTag(selected.id, toInput(editForm));
			toastStore.push('success', '更新しました');
			selected = updated;
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) editErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	async function handleDelete(): Promise<void> {
		if (!selected) return;
		if (!window.confirm(`${selected.name} を削除しますか？`)) return;
		try {
			await deleteTag(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	// --- T11-1: 連続登録 (docs/ux-plan.md §3) ------------------------------
	//
	// 名前パターン・開始番号・開始アドレス・点数・共通設定から
	// `generateContinuousTags`（純関数、$lib/banto/continuousRegistration.ts）
	// でプレビュー行を組み立て、確認後に一括 API を叩く。連続登録は PLC
	// アドレスを前提とする機能のため tagKind は常に 'plc'（TagInput 側の
	// 既定と同じ、フォーム自体に種別選択は出さない）。

	interface ContinuousFormState {
		collectionGroupId: string;
		namePattern: string;
		startNumber: string;
		startAddress: string;
		count: string;
		dataType: TagDataType;
		stringLength: string;
		unit: string;
		decimals: string;
		rawLo: string;
		rawHi: string;
		engLo: string;
		engHi: string;
		thresholdH: string;
		thresholdHh: string;
		thresholdL: string;
		thresholdLl: string;
		enabled: boolean;
		writable: boolean;
	}

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

	/** 生成に必要な最低限の項目が埋まるまでは `null`（エラー表示を急がない）。 */
	function continuousParams(form: ContinuousFormState): ContinuousRegistrationParams | null {
		if (
			form.collectionGroupId === '' ||
			form.namePattern.trim() === '' ||
			form.startAddress.trim() === '' ||
			form.count.trim() === ''
		) {
			return null;
		}
		return {
			collectionGroupId: Number(form.collectionGroupId),
			namePattern: form.namePattern,
			startNumber: Number(form.startNumber) || 0,
			startAddress: form.startAddress,
			count: Number(form.count),
			dataType: form.dataType,
			stringLength: form.dataType === 'string' ? (optNum(form.stringLength) ?? null) : null,
			unit: form.unit === '' ? undefined : form.unit,
			decimals: Number(form.decimals),
			rawLo: optNum(form.rawLo) ?? null,
			rawHi: optNum(form.rawHi) ?? null,
			engLo: optNum(form.engLo) ?? null,
			engHi: optNum(form.engHi) ?? null,
			thresholdH: optNum(form.thresholdH) ?? null,
			thresholdHh: optNum(form.thresholdHh) ?? null,
			thresholdL: optNum(form.thresholdL) ?? null,
			thresholdLl: optNum(form.thresholdLl) ?? null,
			enabled: form.enabled,
			writable: form.writable
		};
	}

	/** 入力が変わるたびに再計算される、適用前プレビュー(設計「適用前にプレビュー表示」)。 */
	let continuousPreview: ContinuousRegistrationResult | null = $derived.by(() => {
		const params = continuousParams(continuousForm);
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

{#snippet tagFields(form: FormState, errors: Record<string, string>)}
	<div class="form-grid">
		<label class="field">
			名前
			<input type="text" bind:value={form.name} />
			{#if errors.name}<span class="err">{errors.name}</span>{/if}
		</label>
		<label class="field">
			収集グループ
			<select bind:value={form.collectionGroupId}>
				<option value="" disabled>選択してください</option>
				{#each groupsFor(form.tagKind) as group (group.id)}
					<option value={String(group.id)}>{group.name}</option>
				{/each}
			</select>
			{#if form.tagKind === 'computed'}
				<span class="hint">{CALC_CONNECTION_NAME} 接続配下のグループのみ選択できます。</span>
			{:else if form.tagKind === 'internal'}
				<span class="hint">{MEM_CONNECTION_NAME} 接続配下のグループのみ選択できます。</span>
			{/if}
			{#if errors.collectionGroupId}<span class="err">{errors.collectionGroupId}</span>{/if}
		</label>
		<label class="field">
			タグ種別
			<select
				bind:value={form.tagKind}
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
			{#if errors.tagKind}<span class="err">{errors.tagKind}</span>{/if}
		</label>
		{#if form.tagKind === 'plc'}
			<label class="field">
				アドレス
				<input type="text" bind:value={form.address} placeholder="D100（ビット: D100.5）" />
				<span class="hint"
					>ワードデバイスの特定ビットを読み書きするときは「D100.5」のように「.」+ビット位置（0〜15、Modbus
					は「40001.3」）を付けます。「D100.5」でワードの5ビット目。ビット指定アドレスは data_type =
					bit のタグでのみ使えます。</span
				>
				{#if errors.address}<span class="err">{errors.address}</span>{/if}
			</label>
		{/if}
		{#if form.tagKind === 'computed'}
			<label class="field wide">
				式（expression）
				<textarea
					bind:value={form.expression}
					rows="2"
					placeholder="(line1.fast.a + line1.fast.b) / 2"></textarea>
				<span class="hint"
					>四則・比較・論理・if(c,a,b)・min/max/abs/round/clamp/bit(tag,n)。参照する外部名は他タグ
					（plc/computed/internal）の完全名。</span
				>
				{#if errors.expression}<span class="err">{errors.expression}</span>{/if}
			</label>
		{/if}
		<label class="field">
			データ型
			<select bind:value={form.dataType}>
				{#each dataTypeOptions as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.dataType}<span class="err">{errors.dataType}</span>{/if}
		</label>
		{#if form.dataType === 'string'}
			<label class="field">
				文字列長（word数）
				<input
					type="number"
					min={MIN_STRING_LENGTH}
					max={MAX_STRING_LENGTH}
					bind:value={form.stringLength}
				/>
				<span class="hint">{MIN_STRING_LENGTH}〜{MAX_STRING_LENGTH} word（1 word = 2バイト）。</span
				>
				{#if errors.stringLength}<span class="err">{errors.stringLength}</span>{/if}
			</label>
		{/if}
		<label class="field">
			単位
			<input type="text" bind:value={form.unit} placeholder="℃" />
			{#if errors.unit}<span class="err">{errors.unit}</span>{/if}
		</label>
		<label class="field">
			小数桁数
			<input type="number" min="0" bind:value={form.decimals} />
			{#if errors.decimals}<span class="err">{errors.decimals}</span>{/if}
		</label>
		<label class="field">
			RawLo
			<input type="number" bind:value={form.rawLo} />
			{#if errors.rawLo}<span class="err">{errors.rawLo}</span>{/if}
		</label>
		<label class="field">
			RawHi
			<input type="number" bind:value={form.rawHi} />
			{#if errors.rawHi}<span class="err">{errors.rawHi}</span>{/if}
		</label>
		<label class="field">
			EngLo
			<input type="number" bind:value={form.engLo} />
			{#if errors.engLo}<span class="err">{errors.engLo}</span>{/if}
		</label>
		<label class="field">
			EngHi
			<input type="number" bind:value={form.engHi} />
			{#if errors.engHi}<span class="err">{errors.engHi}</span>{/if}
		</label>
		<label class="field">
			しきい値 H
			<input type="number" bind:value={form.thresholdH} />
			{#if errors.thresholdH}<span class="err">{errors.thresholdH}</span>{/if}
		</label>
		<label class="field">
			しきい値 HH
			<input type="number" bind:value={form.thresholdHh} />
			{#if errors.thresholdHh}<span class="err">{errors.thresholdHh}</span>{/if}
		</label>
		<label class="field">
			しきい値 L
			<input type="number" bind:value={form.thresholdL} />
			{#if errors.thresholdL}<span class="err">{errors.thresholdL}</span>{/if}
		</label>
		<label class="field">
			しきい値 LL
			<input type="number" bind:value={form.thresholdLl} />
			{#if errors.thresholdLl}<span class="err">{errors.thresholdLl}</span>{/if}
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
		{#if form.tagKind !== 'computed'}
			<label class="field checkbox">
				<input type="checkbox" bind:checked={form.writable} />
				書き込み可（writable）
			</label>
		{/if}
		{#if form.tagKind === 'internal'}
			<label class="field checkbox">
				<input type="checkbox" bind:checked={form.retain} />
				retain（再起動時に最終値を復元）
			</label>
		{/if}
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
	<h2>タグ登録</h2>

	{#if canWrite}
		<div class="mode-toggle">
			<button type="button" class:active={mode === 'single'} onclick={() => (mode = 'single')}
				>通常登録</button
			>
			<button
				type="button"
				class:active={mode === 'continuous'}
				onclick={() => (mode = 'continuous')}>連続登録</button
			>
			<button type="button" class:active={mode === 'csv'} onclick={() => (mode = 'csv')}
				>CSVインポート</button
			>
		</div>
	{/if}

	{#if canWrite && mode === 'single'}
		<section class="create">
			<h3>新規作成</h3>
			{@render tagFields(createForm, createErrors)}
			<button type="button" onclick={handleCreate} disabled={creating || groups.length === 0}
				>作成</button
			>
			{#if groups.length === 0}
				<p class="note">先に 収集グループ を1件以上登録してください。</p>
			{/if}
		</section>
	{/if}

	{#if canWrite && mode === 'continuous'}
		<section class="create">
			<h3>連続登録</h3>
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
					<button type="button" onclick={handleValidateContinuous} disabled={validating}
						>検証</button
					>
					<button
						type="button"
						onclick={handleApplyContinuous}
						disabled={!continuousValidatedFresh || applyingContinuous}>登録</button
					>
					{#if !continuousValidatedFresh}
						<span class="hint"
							>先に「検証」を実行してください（フォームを変更すると再検証が必要）。</span
						>
					{/if}
				</div>
			{/if}
		</section>
	{/if}

	{#if canWrite && mode === 'csv'}
		<section class="create">
			<h3>CSVインポート</h3>
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
					<button type="button" onclick={handleValidateCsv} disabled={csvValidating}>検証</button>
					<button
						type="button"
						onclick={handleApplyCsv}
						disabled={!csvValidatedFresh || csvApplying}>登録</button
					>
					{#if !csvValidatedFresh}
						<span class="hint"
							>先に「検証」を実行してください（ファイルを差し替えると再検証が必要）。</span
						>
					{/if}
				</div>
			{/if}
		</section>
	{/if}

	<section class="list">
		<h3>一覧</h3>
		<div class="actions">
			<button type="button" onclick={handleExportCsv}>CSVエクスポート</button>
		</div>
		<p class="note">
			{canWrite
				? '行をクリックすると下に編集パネルが表示されます。'
				: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
		</p>
		{#if loading && tags.length === 0}
			<p class="loading">読み込み中…</p>
		{:else}
			<div class="grid-wrap">
				<BantoGrid
					rows={tags}
					{columns}
					getRowId={(t) => t.id}
					onRowClick={canWrite ? selectTag : undefined}
				/>
			</div>
		{/if}
	</section>

	{#if selected && canWrite}
		<section class="detail">
			<h3>{selected.name} を編集</h3>
			{@render tagFields(editForm, editErrors)}
			<div class="actions">
				<button type="button" onclick={saveEdit} disabled={saving}>保存</button>
				<button type="button" class="danger" onclick={handleDelete}>削除</button>
			</div>
		</section>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 960px;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	h3 {
		margin: 0 0 0.75rem;
		font-size: 0.95rem;
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
		height: 320px;
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

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
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

	.mode-toggle {
		display: flex;
		gap: 0.5rem;
	}

	.mode-toggle button {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text-muted);
	}

	.mode-toggle button.active {
		background: var(--banto-primary);
		border-color: var(--banto-primary);
		color: var(--banto-text-inverse);
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

	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}
</style>
