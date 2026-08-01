<script lang="ts">
	/**
	 * タグ登録（tags）CRUD 画面（R1-B）。書き込みルールの条件・コピー元が参照
	 * するソースタグの登録。**一覧（BantoGrid）を主役に据えたリストメイン
	 * レイアウト**で、フォーム類はすべてモーダル（ポップアップ）に収める:
	 *
	 * - ツールバーの「新規作成」→ タグフォームのモーダル。一覧の行クリックも
	 *   同じモーダルを編集モード（既存値プリフィル＋削除ボタン付き）で開く。
	 * - 「一括登録（貼り付け）」→ Excel/CSV から貼り付けたテキストを解析して
	 *   プレビューし、既存の createTag を1行ずつ呼ぶ一括登録モーダル
	 *   （1件ずつ個別に監査される。バックエンド変更なし）。
	 * - 「収集グループ管理」→ 収集グループの一覧＋作成・編集・削除のモーダル
	 *   （グループはタグの実装詳細であり独立画面にしない方針は維持しつつ、
	 *   リストメイン化のため常時表示セクションからモーダルへ移動）。
	 *
	 * モーダルの作法は CommandPalette.svelte（本アプリ唯一の既存オーバーレイ）
	 * に合わせる: `{#if}` でマウントするたび新品インスタンス・
	 * role="dialog" aria-modal・Esc で閉じる・開いたら最初の入力にフォーカス。
	 * ただしフォーム入力を失わないよう、パレットと違い外側クリックでは
	 * 閉じない（明示的な ✕ / キャンセル / Esc のみ）。
	 *
	 * 一括貼り付けの解析ルール（manual.md §4.3 に同文を記載）:
	 * - 1行 = 1タグ。列順は 名前, アドレス, データ型, 単位, 小数桁。
	 * - 区切りは行ごとに自動判定: タブを含む行はタブ区切り（Excel からの
	 *   コピー）、含まない行はカンマ区切り（CSV）。
	 * - 先頭行のデータ型セルが有効値（bit/i16/u16/i32/u32/f32）でない場合は
	 *   ヘッダー行とみなして読み飛ばす。
	 * - 名前・アドレス・データ型は必須。単位は空欄可（空 = 単位なし）、
	 *   小数桁は空欄 = 0（入力時は 0〜6 の整数）。
	 * - スケーリング・しきい値は一括登録では設定しない（登録後に行クリックで
	 *   編集）。
	 *
	 * period_ms について: relay-wright のエンジンは自前の固定間隔でポーリング
	 * するため、収集周期は共有レジストリ上のメタデータ（ChronoGazer 等の収集
	 * エンジンが使用）であり、本アプリでは情報表示にとどまる — その旨を
	 * ヘルプテキストに明記する。
	 *
	 * スケーリング（raw/eng の上下限）は banto-tags の検証どおり「4つ全て
	 * 入力するか、全て空にするか」の all-or-nothing。しきい値は
	 * LL ≤ L ≤ H ≤ HH（設定されたもの同士のみ比較）。
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
		createCollectionGroup,
		updateCollectionGroup,
		deleteCollectionGroup,
		listPlcConnections,
		isTagRegistryAvailable,
		ALLOWED_PERIOD_MS,
		DEMO_MODE_MESSAGE,
		type Tag,
		type TagInput,
		type TagDataType,
		type CollectionGroup,
		type CollectionGroupInput,
		type PlcConnection
	} from '$lib/banto/tagRegistryAdmin';

	const dataTypeOptions: TagDataType[] = ['bit', 'i16', 'u16', 'i32', 'u32', 'f32'];

	function periodLabel(ms: number): string {
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60000) return `${ms / 1000}s`;
		return `${ms / 60000}min`;
	}

	const available = isTagRegistryAvailable();
	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	function applyFieldErrors(err: unknown): Record<string, string> | null {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const map: Record<string, string> = {};
			for (const fe of err.body.field_errors) map[fe.field] = fe.message;
			return map;
		}
		return null;
	}

	/**
	 * Svelte action: モーダルを開いた直後に最初の操作可能要素へフォーカスを
	 * 移す（CommandPalette の onMount 自動フォーカスと同じ発想。モーダルは
	 * `{#if}` マウントなので開くたびに発火する）。
	 */
	function focusFirstField(node: HTMLElement): void {
		const el = node.querySelector<HTMLElement>('input, select, textarea, button');
		el?.focus();
	}

	// --- shared data ---
	let tags: Tag[] = $state([]);
	let groups: CollectionGroup[] = $state([]);
	let connections: PlcConnection[] = $state([]);
	let loading = $state(false);

	function connectionName(id: number): string {
		return connections.find((c) => c.id === id)?.name ?? `#${id}`;
	}

	function groupLabel(g: CollectionGroup): string {
		return `${g.name}（${connectionName(g.plcConnectionId)}）`;
	}

	function groupName(id: number): string {
		const g = groups.find((entry) => entry.id === id);
		return g ? g.name : `#${id}`;
	}

	async function reload(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			const [tagList, groupList, connectionList] = await Promise.all([
				listTags(),
				listCollectionGroups(),
				listPlcConnections()
			]);
			tags = tagList;
			groups = groupList;
			connections = connectionList;
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	// --- collection group management (modal; groups are an implementation
	// detail of tags, so no separate screen) ---
	interface GroupFormState {
		name: string;
		plcConnectionId: string;
		periodMs: string;
		enabled: boolean;
	}

	function blankGroupForm(): GroupFormState {
		return { name: '', plcConnectionId: '', periodMs: '1000', enabled: true };
	}

	function groupToInput(form: GroupFormState): CollectionGroupInput {
		return {
			name: form.name,
			plcConnectionId: Number(form.plcConnectionId),
			periodMs: Number(form.periodMs),
			enabled: form.enabled
		};
	}

	let groupModalOpen = $state(false);
	let groupForm = $state(blankGroupForm());
	let groupErrors: Record<string, string> = $state({});
	let groupSaving = $state(false);
	/** null = creating a new group; otherwise the group being edited. */
	let editingGroup: CollectionGroup | null = $state(null);

	function openGroupModal(): void {
		cancelEditGroup();
		groupModalOpen = true;
	}

	function closeGroupModal(): void {
		cancelEditGroup();
		groupModalOpen = false;
	}

	function startEditGroup(g: CollectionGroup): void {
		editingGroup = g;
		groupForm = {
			name: g.name,
			plcConnectionId: String(g.plcConnectionId),
			periodMs: String(g.periodMs),
			enabled: g.enabled
		};
		groupErrors = {};
	}

	function cancelEditGroup(): void {
		editingGroup = null;
		groupForm = blankGroupForm();
		groupErrors = {};
	}

	async function saveGroup(): Promise<void> {
		groupSaving = true;
		groupErrors = {};
		try {
			if (editingGroup) {
				await updateCollectionGroup(editingGroup.id, groupToInput(groupForm));
				toastStore.push('success', '収集グループを更新しました');
			} else {
				await createCollectionGroup(groupToInput(groupForm));
				toastStore.push('success', '収集グループを作成しました');
			}
			cancelEditGroup();
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) groupErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			groupSaving = false;
		}
	}

	async function handleDeleteGroup(g: CollectionGroup): Promise<void> {
		if (!window.confirm(`収集グループ ${g.name} を削除しますか？`)) return;
		try {
			await deleteCollectionGroup(g.id);
			toastStore.push('success', '収集グループを削除しました');
			if (editingGroup?.id === g.id) cancelEditGroup();
			await reload();
		} catch (err) {
			// タグが所属している場合はサービス層の件数入り Validation エラー。
			toastStore.push('error', errorMessage(err));
		}
	}

	// --- tag create/edit (single modal) ---
	/** Editable form state (shared by create + edit). Strings for numeric inputs so empty = unset. */
	interface FormState {
		name: string;
		collectionGroupId: string;
		address: string;
		dataType: TagDataType;
		unit: string;
		decimals: string;
		rawLo: string;
		rawHi: string;
		engLo: string;
		engHi: string;
		thresholdLl: string;
		thresholdL: string;
		thresholdH: string;
		thresholdHh: string;
		enabled: boolean;
	}

	function blankForm(): FormState {
		return {
			name: '',
			collectionGroupId: '',
			address: '',
			dataType: 'i16',
			unit: '',
			decimals: '0',
			rawLo: '',
			rawHi: '',
			engLo: '',
			engHi: '',
			thresholdLl: '',
			thresholdL: '',
			thresholdH: '',
			thresholdHh: '',
			enabled: true
		};
	}

	function formFromTag(t: Tag): FormState {
		return {
			name: t.name,
			collectionGroupId: String(t.collectionGroupId),
			address: t.address,
			dataType: t.dataType,
			unit: t.unit ?? '',
			decimals: String(t.decimals),
			rawLo: t.rawLo === null ? '' : String(t.rawLo),
			rawHi: t.rawHi === null ? '' : String(t.rawHi),
			engLo: t.engLo === null ? '' : String(t.engLo),
			engHi: t.engHi === null ? '' : String(t.engHi),
			thresholdLl: t.thresholdLl === null ? '' : String(t.thresholdLl),
			thresholdL: t.thresholdL === null ? '' : String(t.thresholdL),
			thresholdH: t.thresholdH === null ? '' : String(t.thresholdH),
			thresholdHh: t.thresholdHh === null ? '' : String(t.thresholdHh),
			enabled: t.enabled
		};
	}

	/**
	 * 空欄 = 未設定（null）。フォーム状態は初期値こそ文字列だが、Svelte 5 の
	 * `bind:value` は `type="number"` の入力後に number（空欄は null）を書き
	 * 戻すため、実行時には string | number | null が混在する — string 前提で
	 * `.trim()` すると入力後の保存が TypeError で落ちる（write-targets/
	 * write-rules に元からあった実バグと同型。3画面とも同修正）。
	 */
	function numOrNull(value: string | number | null): number | null {
		if (typeof value === 'number') return Number.isNaN(value) ? null : value;
		if (value === null) return null;
		const trimmed = value.trim();
		return trimmed === '' ? null : Number(trimmed);
	}

	function toInput(form: FormState): TagInput {
		return {
			name: form.name,
			collectionGroupId: Number(form.collectionGroupId),
			address: form.address,
			dataType: form.dataType,
			unit: form.unit.trim() === '' ? null : form.unit,
			decimals: Number(form.decimals),
			rawLo: numOrNull(form.rawLo),
			rawHi: numOrNull(form.rawHi),
			engLo: numOrNull(form.engLo),
			engHi: numOrNull(form.engHi),
			thresholdLl: numOrNull(form.thresholdLl),
			thresholdL: numOrNull(form.thresholdL),
			thresholdH: numOrNull(form.thresholdH),
			thresholdHh: numOrNull(form.thresholdHh),
			enabled: form.enabled
		};
	}

	let tagModalOpen = $state(false);
	/** null = creating a new tag; otherwise the tag being edited. */
	let editingTag: Tag | null = $state(null);
	let tagForm = $state(blankForm());
	let tagErrors: Record<string, string> = $state({});
	let tagSaving = $state(false);

	function openCreateModal(): void {
		editingTag = null;
		tagForm = blankForm();
		tagErrors = {};
		tagModalOpen = true;
	}

	function openEditModal(t: Tag): void {
		editingTag = t;
		tagForm = formFromTag(t);
		tagErrors = {};
		tagModalOpen = true;
	}

	function closeTagModal(): void {
		tagModalOpen = false;
		editingTag = null;
		tagErrors = {};
	}

	async function saveTag(): Promise<void> {
		tagSaving = true;
		tagErrors = {};
		try {
			if (editingTag) {
				await updateTag(editingTag.id, toInput(tagForm));
				toastStore.push('success', '更新しました');
			} else {
				await createTag(toInput(tagForm));
				toastStore.push('success', '作成しました');
			}
			closeTagModal();
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) tagErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			tagSaving = false;
		}
	}

	async function handleDeleteTag(): Promise<void> {
		if (!editingTag) return;
		if (!window.confirm(`${editingTag.name} を削除しますか？`)) return;
		try {
			await deleteTag(editingTag.id);
			toastStore.push('success', '削除しました');
			closeTagModal();
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	// --- bulk paste registration -------------------------------------------
	/** 貼り付けテキストの1データ行（ヘッダー行と空行は除外済み）。 */
	interface BulkRow {
		/** 貼り付けテキスト内の1始まり行番号（プレビュー表示・結果対応付け用）。 */
		line: number;
		name: string;
		address: string;
		/** 入力されたままのデータ型セル（小文字化前）。 */
		dataTypeRaw: string;
		unit: string;
		decimalsRaw: string;
		/** クライアント側検証エラー（空 = 登録可能）。 */
		errors: string[];
	}

	let bulkModalOpen = $state(false);
	let bulkGroupId = $state('');
	let bulkText = $state('');
	let bulkRunning = $state(false);
	/**
	 * 直近の一括登録の行別結果（キー = BulkRow.line）。ok=true は登録済み
	 * （再実行時はスキップ）、ok=false はバックエンドエラー（メッセージを
	 * プレビューに表示し、再実行で再試行）。テキストを編集すると行番号の
	 * 対応が崩れるためクリアする。
	 */
	let bulkRowStatus: Record<number, { ok: boolean; message: string }> = $state({});

	function openBulkModal(): void {
		bulkGroupId = '';
		bulkText = '';
		bulkRowStatus = {};
		bulkModalOpen = true;
	}

	function closeBulkModal(): void {
		bulkModalOpen = false;
	}

	/** 行ごとの区切り自動判定: タブがあればタブ区切り、なければカンマ区切り。 */
	function splitBulkLine(line: string): string[] {
		return (line.includes('\t') ? line.split('\t') : line.split(',')).map((cell) => cell.trim());
	}

	function isValidDataType(value: string): value is TagDataType {
		return (dataTypeOptions as string[]).includes(value);
	}

	const bulkParse = $derived.by((): { rows: BulkRow[]; headerSkipped: boolean } => {
		const rows: BulkRow[] = [];
		let headerSkipped = false;
		let firstDataLine = true;
		const lines = bulkText.split(/\r?\n/);
		for (let i = 0; i < lines.length; i++) {
			if (lines[i].trim() === '') continue;
			const cells = splitBulkLine(lines[i]);
			const [name = '', address = '', dataTypeRaw = '', unit = '', decimalsRaw = ''] = cells;
			const dataType = dataTypeRaw.toLowerCase();
			if (firstDataLine) {
				firstDataLine = false;
				// 先頭行のデータ型セルが有効値でなければヘッダー行とみなして
				// 読み飛ばす（Excel/CSV の見出し行をそのまま貼れるように）。
				if (!isValidDataType(dataType)) {
					headerSkipped = true;
					continue;
				}
			}
			const errors: string[] = [];
			if (name === '') errors.push('名前は必須です');
			if (address === '') errors.push('アドレスは必須です');
			if (!isValidDataType(dataType)) {
				errors.push('データ型は bit / i16 / u16 / i32 / u32 / f32 のいずれかです');
			}
			if (decimalsRaw !== '' && !/^[0-6]$/.test(decimalsRaw)) {
				errors.push('小数桁は 0〜6 の整数です');
			}
			rows.push({ line: i + 1, name, address, dataTypeRaw, unit, decimalsRaw, errors });
		}
		return { rows, headerSkipped };
	});

	const bulkValidRows = $derived(bulkParse.rows.filter((r) => r.errors.length === 0));
	/** 登録済み（ok）を除いた、今回の「登録」で実際に送信される行。 */
	const bulkPendingRows = $derived(bulkValidRows.filter((r) => !bulkRowStatus[r.line]?.ok));

	function bulkRowToInput(row: BulkRow, collectionGroupId: number): TagInput {
		return {
			name: row.name,
			collectionGroupId,
			address: row.address,
			dataType: row.dataTypeRaw.toLowerCase() as TagDataType,
			unit: row.unit === '' ? null : row.unit,
			decimals: row.decimalsRaw === '' ? 0 : Number(row.decimalsRaw),
			enabled: true
		};
	}

	async function runBulkCreate(): Promise<void> {
		if (bulkGroupId === '' || bulkPendingRows.length === 0) return;
		const collectionGroupId = Number(bulkGroupId);
		bulkRunning = true;
		let okCount = 0;
		let failCount = 0;
		const status: Record<number, { ok: boolean; message: string }> = { ...bulkRowStatus };
		try {
			// 1行ずつ既存の createTag を呼ぶ（各行が個別に監査される）。失敗
			// （バックエンドの重複名エラー等）は記録して残りの行を続行する。
			for (const row of bulkPendingRows) {
				try {
					await createTag(bulkRowToInput(row, collectionGroupId));
					okCount++;
					status[row.line] = { ok: true, message: '登録しました' };
				} catch (err) {
					failCount++;
					status[row.line] = { ok: false, message: errorMessage(err) };
				}
				bulkRowStatus = { ...status };
			}
			toastStore.push(
				failCount === 0 ? 'success' : 'error',
				`一括登録: 成功${okCount}件・失敗${failCount}件`
			);
			await reload();
			// 全行成功（かつ検証エラー行もない）なら閉じる。失敗行がある場合は
			// バックエンドのエラーメッセージ付きで残し、修正して再実行できる。
			if (failCount === 0 && bulkValidRows.length === bulkParse.rows.length) {
				closeBulkModal();
			}
		} finally {
			bulkRunning = false;
		}
	}

	// --- modal escape handling (CommandPalette と同じく Esc で閉じる) ---
	function handleWindowKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Escape') return;
		if (bulkModalOpen) {
			if (!bulkRunning) closeBulkModal();
			event.preventDefault();
		} else if (tagModalOpen) {
			if (!tagSaving) closeTagModal();
			event.preventDefault();
		} else if (groupModalOpen) {
			if (!groupSaving) closeGroupModal();
			event.preventDefault();
		}
	}

	const columns: GridColumn<Tag>[] = [
		{ id: 'id', header: 'ID', accessor: 'id', width: 60, align: 'right' },
		{
			id: 'name',
			header: '名前',
			accessor: 'name',
			width: 150,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'collectionGroup',
			header: '収集グループ',
			accessor: (t) => groupName(t.collectionGroupId),
			width: 130
		},
		{ id: 'address', header: 'アドレス', accessor: 'address', width: 100 },
		{ id: 'dataType', header: '型', accessor: 'dataType', width: 70 },
		{ id: 'unit', header: '単位', accessor: 'unit', width: 80 },
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		}
	];
</script>

<svelte:window onkeydown={handleWindowKeydown} />

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
				<option value="">選択してください</option>
				{#each groups as g (g.id)}
					<option value={String(g.id)}>{groupLabel(g)}</option>
				{/each}
			</select>
			{#if errors.collectionGroupId}<span class="err">{errors.collectionGroupId}</span>{/if}
		</label>
		<label class="field">
			アドレス
			<input type="text" bind:value={form.address} placeholder="D100" />
			<span class="hint">SLMP 記法（例: D100 / M10 / X1A）</span>
			{#if errors.address}<span class="err">{errors.address}</span>{/if}
		</label>
		<label class="field">
			データ型
			<select bind:value={form.dataType}>
				{#each dataTypeOptions as dt (dt)}
					<option value={dt}>{dt}</option>
				{/each}
			</select>
			{#if errors.dataType}<span class="err">{errors.dataType}</span>{/if}
		</label>
		<label class="field">
			単位
			<input type="text" bind:value={form.unit} />
		</label>
		<label class="field">
			小数桁
			<input type="number" min="0" max="6" bind:value={form.decimals} />
			{#if errors.decimals}<span class="err">{errors.decimals}</span>{/if}
		</label>
		<label class="field">
			raw下限
			<input type="number" bind:value={form.rawLo} />
		</label>
		<label class="field">
			raw上限
			<input type="number" bind:value={form.rawHi} />
		</label>
		<label class="field">
			eng下限
			<input type="number" bind:value={form.engLo} />
		</label>
		<label class="field">
			eng上限
			<input type="number" bind:value={form.engHi} />
		</label>
		<label class="field">
			しきい値LL
			<input type="number" bind:value={form.thresholdLl} />
			{#if errors.thresholdLl}<span class="err">{errors.thresholdLl}</span>{/if}
		</label>
		<label class="field">
			しきい値L
			<input type="number" bind:value={form.thresholdL} />
			{#if errors.thresholdL}<span class="err">{errors.thresholdL}</span>{/if}
		</label>
		<label class="field">
			しきい値H
			<input type="number" bind:value={form.thresholdH} />
			{#if errors.thresholdH}<span class="err">{errors.thresholdH}</span>{/if}
		</label>
		<label class="field">
			しきい値HH
			<input type="number" bind:value={form.thresholdHh} />
			{#if errors.thresholdHh}<span class="err">{errors.thresholdHh}</span>{/if}
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
	</div>
	<p class="note">
		スケーリング（raw/eng の上下限）は 4 つすべて入力するか、すべて空にしてください （空 =
		スケーリングなし）。しきい値は LL ≤ L ≤ H ≤ HH の順（設定した項目のみ比較）。
	</p>
	{#if errors.scaling}<p class="err">{errors.scaling}</p>{/if}
{/snippet}

<div class="page">
	<h2>タグ登録</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはレジストリDBがないため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<div class="toolbar">
			{#if canWrite}
				<button type="button" onclick={openCreateModal}>新規作成</button>
				<button type="button" class="ghost" onclick={openBulkModal}>一括登録（貼り付け）</button>
			{/if}
			<button type="button" class="ghost" onclick={openGroupModal}>収集グループ管理</button>
			<span class="toolbar-note">
				{canWrite
					? '行をクリックすると編集ポップアップが開きます。'
					: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
			</span>
		</div>

		{#if canWrite && groups.length === 0 && !loading}
			<p class="note">
				収集グループがまだありません。タグを登録する前に「収集グループ管理」から作成してください。
			</p>
		{/if}

		<section class="list">
			{#if loading && tags.length === 0}
				<p class="loading">読み込み中…</p>
			{:else}
				<div class="grid-wrap">
					<BantoGrid
						rows={tags}
						{columns}
						getRowId={(t) => t.id}
						onRowClick={canWrite ? openEditModal : undefined}
					/>
				</div>
			{/if}
		</section>
	{/if}
</div>

{#if tagModalOpen}
	<div class="overlay">
		<div
			class="modal"
			role="dialog"
			aria-modal="true"
			aria-label={editingTag ? `${editingTag.name} を編集` : 'タグの新規作成'}
			use:focusFirstField
		>
			<div class="modal-head">
				<h3>{editingTag ? `${editingTag.name} を編集` : 'タグの新規作成'}</h3>
				<button type="button" class="close" aria-label="閉じる" onclick={closeTagModal}>×</button>
			</div>
			{@render tagFields(tagForm, tagErrors)}
			<div class="actions">
				<button type="button" onclick={saveTag} disabled={tagSaving}>
					{editingTag ? '保存' : '作成'}
				</button>
				{#if editingTag}
					<button type="button" class="danger" onclick={handleDeleteTag}>削除</button>
				{/if}
				<button type="button" class="ghost" onclick={closeTagModal}>キャンセル</button>
			</div>
		</div>
	</div>
{/if}

{#if bulkModalOpen}
	<div class="overlay">
		<div
			class="modal wide"
			role="dialog"
			aria-modal="true"
			aria-label="一括登録（貼り付け）"
			use:focusFirstField
		>
			<div class="modal-head">
				<h3>一括登録（貼り付け）</h3>
				<button
					type="button"
					class="close"
					aria-label="閉じる"
					onclick={closeBulkModal}
					disabled={bulkRunning}>×</button
				>
			</div>
			<p class="note">
				Excel（タブ区切り）や CSV（カンマ区切り）からコピーした行を貼り付けます。 列の順は
				<strong>名前, アドレス, データ型, 単位, 小数桁</strong>
				（名前・アドレス・データ型は必須。単位は空欄可、小数桁は空欄 = 0）。 先頭行のデータ型セルが有効値でない場合はヘッダー行として読み飛ばします。
			</p>
			<label class="field">
				収集グループ（貼り付けた全行に適用）
				<select bind:value={bulkGroupId} disabled={bulkRunning}>
					<option value="">選択してください</option>
					{#each groups as g (g.id)}
						<option value={String(g.id)}>{groupLabel(g)}</option>
					{/each}
				</select>
			</label>
			<label class="field">
				貼り付け
				<textarea
					rows="6"
					bind:value={bulkText}
					oninput={() => (bulkRowStatus = {})}
					disabled={bulkRunning}
					placeholder={'温度センサ\tD100\ti16\t℃\t1\n運転状態\tM10\tbit\n圧力センサ,D110,i16,kPa,0'}
				></textarea>
			</label>
			{#if bulkParse.rows.length > 0 || bulkParse.headerSkipped}
				<p class="bulk-summary">
					{bulkParse.rows.length}件中{bulkPendingRows.length}件登録可能
					{#if bulkParse.headerSkipped}（先頭行はヘッダー行として無視）{/if}
				</p>
				<div class="bulk-preview-wrap">
					<table class="bulk-preview">
						<thead>
							<tr>
								<th>行</th>
								<th>名前</th>
								<th>アドレス</th>
								<th>データ型</th>
								<th>単位</th>
								<th>小数桁</th>
								<th>状態</th>
							</tr>
						</thead>
						<tbody>
							{#each bulkParse.rows as row (row.line)}
								{@const result = bulkRowStatus[row.line]}
								<tr class:invalid={row.errors.length > 0 || result?.ok === false}>
									<td class="num">{row.line}</td>
									<td>{row.name}</td>
									<td>{row.address}</td>
									<td>{row.dataTypeRaw}</td>
									<td>{row.unit}</td>
									<td>{row.decimalsRaw === '' ? '0（既定）' : row.decimalsRaw}</td>
									<td>
										{#if result}
											<span class={result.ok ? 'ok' : 'err'}>{result.message}</span>
										{:else if row.errors.length > 0}
											<span class="err">{row.errors.join('、')}</span>
										{:else}
											<span class="ok">登録可能</span>
										{/if}
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
			<div class="actions">
				<button
					type="button"
					onclick={runBulkCreate}
					disabled={bulkRunning || bulkGroupId === '' || bulkPendingRows.length === 0}
				>
					{bulkRunning ? '登録中…' : `登録（${bulkPendingRows.length}件）`}
				</button>
				<button type="button" class="ghost" onclick={closeBulkModal} disabled={bulkRunning}>
					閉じる
				</button>
			</div>
		</div>
	</div>
{/if}

{#if groupModalOpen}
	<div class="overlay">
		<div
			class="modal wide"
			role="dialog"
			aria-modal="true"
			aria-label="収集グループ管理"
			use:focusFirstField
		>
			<div class="modal-head">
				<h3>収集グループ管理</h3>
				<button type="button" class="close" aria-label="閉じる" onclick={closeGroupModal}>×</button>
			</div>
			<p class="note">
				タグは必ずいずれかの収集グループ（PLC接続への所属単位）に属します。
				収集周期（period）は共有レジストリのメタデータで、本アプリのエンジンは自前の
				固定間隔でポーリングするため、ここでは参考情報です。
			</p>
			{#if groups.length === 0}
				<p class="note">
					収集グループがまだありません。{canWrite
						? 'タグを登録する前に、下のフォームから作成してください。'
						: '編集者以上の権限で作成できます。'}
				</p>
			{:else}
				<table class="group-table">
					<thead>
						<tr>
							<th>ID</th>
							<th>名前</th>
							<th>PLC接続</th>
							<th>収集周期</th>
							<th>有効</th>
							{#if canWrite}<th></th>{/if}
						</tr>
					</thead>
					<tbody>
						{#each groups as g (g.id)}
							<tr class:editing={editingGroup?.id === g.id}>
								<td class="num">{g.id}</td>
								<td>{g.name}</td>
								<td>{connectionName(g.plcConnectionId)}</td>
								<td>{periodLabel(g.periodMs)}</td>
								<td>{g.enabled ? 'はい' : 'いいえ'}</td>
								{#if canWrite}
									<td class="row-actions">
										<button type="button" class="small" onclick={() => startEditGroup(g)}>
											編集
										</button>
										<button type="button" class="small danger" onclick={() => handleDeleteGroup(g)}>
											削除
										</button>
									</td>
								{/if}
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
			{#if canWrite}
				<div class="group-form">
					<span class="group-form-title">
						{editingGroup ? `${editingGroup.name} を編集` : '新規グループ'}
					</span>
					<div class="form-grid">
						<label class="field">
							名前
							<input type="text" bind:value={groupForm.name} />
							{#if groupErrors.name}<span class="err">{groupErrors.name}</span>{/if}
						</label>
						<label class="field">
							PLC接続
							<select bind:value={groupForm.plcConnectionId}>
								<option value="">選択してください</option>
								{#each connections as c (c.id)}
									<option value={String(c.id)}>{c.name}（{c.protocol}）</option>
								{/each}
							</select>
							{#if groupErrors.plcConnectionId}<span class="err">{groupErrors.plcConnectionId}</span
								>{/if}
						</label>
						<label class="field">
							収集周期
							<select bind:value={groupForm.periodMs}>
								{#each ALLOWED_PERIOD_MS as ms (ms)}
									<option value={String(ms)}>{periodLabel(ms)}</option>
								{/each}
							</select>
							{#if groupErrors.periodMs}<span class="err">{groupErrors.periodMs}</span>{/if}
						</label>
						<label class="field checkbox">
							<input type="checkbox" bind:checked={groupForm.enabled} />
							有効
						</label>
					</div>
					<div class="actions">
						<button type="button" onclick={saveGroup} disabled={groupSaving}>
							{editingGroup ? '保存' : '作成'}
						</button>
						{#if editingGroup}
							<button type="button" class="ghost" onclick={cancelEditGroup}>キャンセル</button>
						{/if}
					</div>
				</div>
			{/if}
		</div>
	</div>
{/if}

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
		margin: 0;
		font-size: 0.95rem;
	}

	.toolbar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	.toolbar-note {
		color: var(--banto-text-muted);
		font-size: 0.8rem;
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
		height: 480px;
	}

	/* --- modal chrome (CommandPalette.svelte のオーバーレイと同作法) --- */
	.overlay {
		position: fixed;
		inset: 0;
		z-index: 1000;
		display: flex;
		justify-content: center;
		align-items: flex-start;
		padding: 6vh 1rem 1rem;
		background: rgba(0, 0, 0, 0.35);
	}

	.modal {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		width: min(720px, 100%);
		max-height: 86vh;
		overflow-y: auto;
		background: var(--banto-surface-raised, var(--banto-surface));
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		box-shadow: 0 12px 40px rgba(0, 0, 0, 0.3);
		padding: 1rem 1.25rem;
		/* Glass preset (spec M12): no-op under standard (--banto-backdrop: none). */
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	.modal.wide {
		width: min(820px, 100%);
	}

	.modal-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}

	button.close {
		background: transparent;
		border: none;
		color: var(--banto-text-muted);
		font-size: 1.1rem;
		line-height: 1;
		padding: 0.2rem 0.4rem;
	}

	button.close:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-text) 8%, transparent);
		color: var(--banto-text);
	}

	/* --- bulk paste --- */
	textarea {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-family: inherit;
		font-size: 0.8rem;
		resize: vertical;
	}

	.bulk-summary {
		margin: 0;
		font-size: 0.8rem;
		font-weight: 600;
	}

	.bulk-preview-wrap {
		overflow-x: auto;
	}

	.bulk-preview {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.8rem;
	}

	.bulk-preview th,
	.bulk-preview td {
		text-align: left;
		padding: 0.35rem 0.5rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.bulk-preview th {
		color: var(--banto-text-muted);
		font-weight: 600;
	}

	.bulk-preview td.num {
		text-align: right;
	}

	.bulk-preview tr.invalid td {
		background: color-mix(in srgb, var(--banto-danger) 8%, transparent);
	}

	.ok {
		color: var(--banto-success, var(--banto-primary));
		font-size: 0.75rem;
	}

	/* --- collection groups (modal) --- */
	.group-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.8rem;
		margin-bottom: 0.75rem;
	}

	.group-table th,
	.group-table td {
		text-align: left;
		padding: 0.35rem 0.5rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.group-table th {
		color: var(--banto-text-muted);
		font-weight: 600;
	}

	.group-table td.num {
		text-align: right;
	}

	.group-table tr.editing td {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.row-actions {
		display: flex;
		gap: 0.4rem;
		justify-content: flex-end;
	}

	.group-form {
		border-top: 1px solid var(--banto-border);
		padding-top: 0.75rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.group-form-title {
		font-size: 0.85rem;
		font-weight: 600;
	}

	.form-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
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

	button.small {
		padding: 0.3rem 0.6rem;
		font-size: 0.75rem;
	}

	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}

	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}

	button.ghost {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
	}

	button.ghost:hover {
		background: color-mix(in srgb, var(--banto-text) 8%, transparent);
	}
</style>
