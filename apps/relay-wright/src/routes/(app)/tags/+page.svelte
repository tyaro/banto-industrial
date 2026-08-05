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
	 * - 「連続登録」→ 開始デバイス（SLMP 記法）と件数から連番アドレスを生成
	 *   して登録するモーダル。名前はアドレス文字列そのものを自動割り付け
	 *   （例: D100）。デバイス記法の解釈は $lib/slmpDevice.ts
	 *   （crates/banto-plc/src/slmp/address.rs のミラー）に依存。登録の実行は
	 *   一括貼り付けと同じ runCreateLoop（1行ずつ createTag・失敗行は続行）。
	 * - 「収集グループ管理」→ 収集グループの一覧＋作成・編集・削除のモーダル
	 *   （グループはタグの実装詳細であり独立画面にしない方針は維持しつつ、
	 *   リストメイン化のため常時表示セクションからモーダルへ移動）。グループの
	 *   削除は**カスケード**（feature/easy-delete）: 所属タグごと1トランザク
	 *   ションで削除し、確認ダイアログに巻き添え件数を出す。
	 * - 一覧グリッドに「選択」（チェックボックス風の複数選択）と「削除」
	 *   （行ごとの1クリック削除）の列（feature/easy-delete）。選択中はツール
	 *   バーに「選択削除（N件）」が現れ、confirm 1回で deleteTag を1件ずつ
	 *   呼ぶ（各件が個別に監査され、失敗行があっても続行）。BantoGrid は
	 *   ボタンセルを持たないため実装はテキストセル + capture 委譲
	 *   （script 側 handleGridClickCapture のコメント参照）。
	 *
	 * モーダルの作法は CommandPalette.svelte（本アプリ唯一の既存オーバーレイ）
	 * に合わせる: `{#if}` でマウントするたび新品インスタンス・
	 * role="dialog" aria-modal・Esc で閉じる・開いたら最初の入力にフォーカス。
	 * ただしフォーム入力を失わないよう、パレットと違い外側クリックでは
	 * 閉じない（明示的な ✕ / キャンセル / Esc のみ）。
	 *
	 * 一括貼り付けの解析ルール（manual.md §4.3 に同文を記載）:
	 * - 1行 = 1タグ。列順は 名前, アドレス, データ型, 単位, 小数桁, 文字列長。
	 * - 区切りは行ごとに自動判定: タブを含む行はタブ区切り（Excel からの
	 *   コピー）、含まない行はカンマ区切り（CSV）。
	 * - 先頭行のデータ型セルが有効値（bit/i16/u16/i32/u32/f32/string）でない
	 *   場合はヘッダー行とみなして読み飛ばす。
	 * - 名前・アドレス・データ型は必須。単位は空欄可（空 = 単位なし）、
	 *   小数桁は空欄 = 0（入力時は 0〜6 の整数）。文字列長（6列目）は string
	 *   型のとき必須（1〜128 語）・数値型では空欄。
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
	import { BantoGrid, GridState, filterRows, sortRows, type GridColumn } from '@banto/grid-svelte';
	import { SvelteSet } from 'svelte/reactivity';
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
		previewCollectionGroupCascade,
		cascadeDeleteCollectionGroup,
		listPlcConnections,
		isTagRegistryAvailable,
		ALLOWED_PERIOD_MS,
		MIN_STRING_LENGTH,
		MAX_STRING_LENGTH,
		DEMO_MODE_MESSAGE,
		type Tag,
		type TagInput,
		type TagDataType,
		type CollectionGroup,
		type CollectionGroupInput,
		type PlcConnection
	} from '$lib/banto/tagRegistryAdmin';
	import {
		parseSlmpDevice,
		formatSlmpDevice,
		SLMP_MAX_DEVICE_NUMBER,
		type SlmpDeviceInfo
	} from '$lib/slmpDevice';

	const dataTypeOptions: TagDataType[] = ['bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'string'];

	/** A `string` tag has a word-length instead of scaling/thresholds (S2 文字列タグ). */
	function isStringType(dataType: TagDataType): boolean {
		return dataType === 'string';
	}

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
			// 消えたタグを選択集合から掃除する（削除・他クライアントの変更後）。
			const alive = new Set(tagList.map((t) => t.id));
			for (const id of [...selectedIds]) {
				if (!alive.has(id)) selectedIds.delete(id);
			}
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

	/**
	 * グループ削除はカスケード（feature/easy-delete）: 所属タグごと 1 トラン
	 * ザクションで削除する。事前に cascade-preview で巻き添え件数（タグ、
	 * および参照が外れる書き込みルール）を取得して確認ダイアログに出す。
	 */
	async function handleDeleteGroup(g: CollectionGroup): Promise<void> {
		let tagCount: number;
		let ruleCount: number;
		try {
			const preview = await previewCollectionGroupCascade(g.id);
			tagCount = preview.tags;
			ruleCount = preview.writeRules;
		} catch (err) {
			toastStore.push('error', errorMessage(err));
			return;
		}
		const lines =
			tagCount > 0
				? [
						`収集グループ ${g.name} を削除すると、所属タグ ${tagCount} 件も削除されます。`,
						...(ruleCount > 0
							? [
									`これらのタグを参照する書き込みルール ${ruleCount} 件は参照先を失い無効になります（ルール自体は削除されません）。`
								]
							: []),
						'削除しますか？'
					].join('\n')
				: `収集グループ ${g.name} を削除しますか？`;
		if (!window.confirm(lines)) return;
		try {
			const summary = await cascadeDeleteCollectionGroup(g.id);
			toastStore.push(
				'success',
				summary.tags > 0
					? `収集グループを削除しました（タグ${summary.tags}件を含む）`
					: '収集グループを削除しました'
			);
			if (editingGroup?.id === g.id) cancelEditGroup();
			await reload();
		} catch (err) {
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
		stringLength: string;
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
			stringLength: '',
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
			stringLength: t.stringLength === null ? '' : String(t.stringLength),
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
		const string = isStringType(form.dataType);
		// A string tag carries a word-length and neither scaling nor thresholds
		// (a raw/eng mapping or a numeric alarm over SJIS text is meaningless);
		// send NULL for the fields the backend forbids for the type so a stray
		// value never trips validation.
		return {
			name: form.name,
			collectionGroupId: Number(form.collectionGroupId),
			address: form.address,
			dataType: form.dataType,
			stringLength: string ? numOrNull(form.stringLength) : null,
			unit: form.unit.trim() === '' ? null : form.unit,
			decimals: Number(form.decimals),
			rawLo: string ? null : numOrNull(form.rawLo),
			rawHi: string ? null : numOrNull(form.rawHi),
			engLo: string ? null : numOrNull(form.engLo),
			engHi: string ? null : numOrNull(form.engHi),
			thresholdLl: string ? null : numOrNull(form.thresholdLl),
			thresholdL: string ? null : numOrNull(form.thresholdL),
			thresholdH: string ? null : numOrNull(form.thresholdH),
			thresholdHh: string ? null : numOrNull(form.thresholdHh),
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
		/** 文字列型の占有ワード数セル（S2 文字列タグ。6列目、数値型では空）。 */
		stringLengthRaw: string;
		/** クライアント側検証エラー（空 = 登録可能）。 */
		errors: string[];
	}

	let bulkModalOpen = $state(false);
	let bulkGroupId = $state('');
	let bulkText = $state('');
	let bulkRunning = $state(false);
	// placeholder はタブ/改行のエスケープを含むため属性リテラルでは書けず、
	// テンプレート内の `{'...'}` は svelte/no-useless-mustaches に当たる。
	// script 側の定数にして参照する。
	const BULK_PLACEHOLDER =
		'温度センサ\tD100\ti16\t℃\t1\n運転状態\tM10\tbit\n品番\tD300\tstring\t\t\t4\n圧力センサ,D110,i16,kPa,0';
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
			const [
				name = '',
				address = '',
				dataTypeRaw = '',
				unit = '',
				decimalsRaw = '',
				stringLengthRaw = ''
			] = cells;
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
				errors.push('データ型は bit / i16 / u16 / i32 / u32 / f32 / string のいずれかです');
			}
			if (decimalsRaw !== '' && !/^[0-6]$/.test(decimalsRaw)) {
				errors.push('小数桁は 0〜6 の整数です');
			}
			// 文字列型は 6 列目に文字列長（1〜128 語）が必須。数値型では空欄に
			// する（バックエンドが数値型の string_length を拒否するため）。
			if (dataType === 'string') {
				const len = Number(stringLengthRaw);
				if (
					stringLengthRaw === '' ||
					!Number.isInteger(len) ||
					len < MIN_STRING_LENGTH ||
					len > MAX_STRING_LENGTH
				) {
					errors.push(`文字列長は ${MIN_STRING_LENGTH}〜${MAX_STRING_LENGTH} の整数です（6列目）`);
				}
			} else if (stringLengthRaw !== '') {
				errors.push('文字列長は string 型でのみ設定できます（6列目）');
			}
			rows.push({
				line: i + 1,
				name,
				address,
				dataTypeRaw,
				unit,
				decimalsRaw,
				stringLengthRaw,
				errors
			});
		}
		return { rows, headerSkipped };
	});

	const bulkValidRows = $derived(bulkParse.rows.filter((r) => r.errors.length === 0));
	/** 登録済み（ok）を除いた、今回の「登録」で実際に送信される行。 */
	const bulkPendingRows = $derived(bulkValidRows.filter((r) => !bulkRowStatus[r.line]?.ok));

	function bulkRowToInput(row: BulkRow, collectionGroupId: number): TagInput {
		const dataType = row.dataTypeRaw.toLowerCase() as TagDataType;
		return {
			name: row.name,
			collectionGroupId,
			address: row.address,
			dataType,
			stringLength: dataType === 'string' ? Number(row.stringLengthRaw) : null,
			unit: row.unit === '' ? null : row.unit,
			decimals: row.decimalsRaw === '' ? 0 : Number(row.decimalsRaw),
			enabled: true
		};
	}

	/** 行別の登録結果（キー = 行番号 / 連番 index）。 */
	type RowResult = { ok: boolean; message: string };

	/**
	 * 1行ずつ既存の createTag を呼ぶ共通ループ（一括貼り付け・連続登録で
	 * 共用。各行が個別に監査される）。失敗（バックエンドの重複名エラー等）
	 * は記録して残りの行を続行し、進捗が見えるよう1行ごとに publish で
	 * 行別結果マップを反映する。
	 */
	async function runCreateLoop<T>(
		rows: readonly T[],
		keyOf: (row: T) => number,
		inputOf: (row: T) => TagInput,
		initialStatus: Record<number, RowResult>,
		publish: (status: Record<number, RowResult>) => void
	): Promise<{ okCount: number; failCount: number }> {
		let okCount = 0;
		let failCount = 0;
		const status = { ...initialStatus };
		for (const row of rows) {
			try {
				await createTag(inputOf(row));
				okCount++;
				status[keyOf(row)] = { ok: true, message: '登録しました' };
			} catch (err) {
				failCount++;
				status[keyOf(row)] = { ok: false, message: errorMessage(err) };
			}
			publish({ ...status });
		}
		return { okCount, failCount };
	}

	async function runBulkCreate(): Promise<void> {
		if (bulkGroupId === '' || bulkPendingRows.length === 0) return;
		const collectionGroupId = Number(bulkGroupId);
		bulkRunning = true;
		try {
			const { okCount, failCount } = await runCreateLoop(
				bulkPendingRows,
				(row) => row.line,
				(row) => bulkRowToInput(row, collectionGroupId),
				bulkRowStatus,
				(status) => (bulkRowStatus = status)
			);
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

	// --- sequential registration (連続登録) ------------------------------
	/**
	 * 一度に登録できる件数の上限。SLMP 上は先のアドレスまでいくらでも
	 * 生成できてしまうが、タイプミス（件数欄に桁を1つ多く入れる等）で
	 * 数千件を誤登録すると片付けが大変なので、1回の操作は256件までに制限
	 * する（それ以上は複数回に分ける）。
	 */
	const SEQ_MAX_COUNT = 256;
	/** 32ビット型は連続する2ワードを占有するため、アドレスを2番地刻みで生成する。 */
	const SEQ_STEP2_TYPES: readonly TagDataType[] = ['i32', 'u32', 'f32'];

	/** 生成される1タグ（名前 = アドレス文字列の自動割り付け）。 */
	interface SeqRow {
		/** 0始まりの連番（行別結果の対応付けキー）。 */
		index: number;
		name: string;
		address: string;
		/** 既存タグと同名（サーバー側で重複エラーになる見込み）の警告。 */
		duplicate: boolean;
	}

	let seqModalOpen = $state(false);
	let seqGroupId = $state('');
	let seqStart = $state('');
	/** type="number" の bind:value は入力後 number | null を書き戻す（numOrNull 参照）。 */
	let seqCount: string | number | null = $state('10');
	let seqDataType: TagDataType = $state('i16');
	/** 文字列型の占有ワード数（S2 文字列タグ。アドレスの刻み幅にもなる）。 */
	let seqStringLength: string | number | null = $state('4');
	let seqUnit = $state('');
	let seqDecimals: string | number | null = $state('0');
	let seqEnabled = $state(true);
	let seqRunning = $state(false);
	/** 直近の連続登録の行別結果（キー = SeqRow.index）。一括貼り付けと同運用。 */
	let seqRowStatus: Record<number, RowResult> = $state({});

	function openSeqModal(): void {
		seqGroupId = '';
		seqStart = '';
		seqCount = '10';
		seqDataType = 'i16';
		seqStringLength = '4';
		seqUnit = '';
		seqDecimals = '0';
		seqEnabled = true;
		seqRowStatus = {};
		seqModalOpen = true;
	}

	function closeSeqModal(): void {
		seqModalOpen = false;
	}

	/** 入力を変えると連番と行の対応が崩れるため、行別結果をクリアする。 */
	function resetSeqStatus(): void {
		seqRowStatus = {};
	}

	const existingTagNames = $derived(new Set(tags.map((t) => t.name)));

	const seqPlan = $derived.by(
		(): {
			rows: SeqRow[];
			step: number;
			/** 文字列型の占有ワード数（生成タグに渡す）。数値型では null。 */
			stringLength: number | null;
			device: SlmpDeviceInfo | null;
			/** 生成をブロックするエラー（null = 生成可能）。 */
			error: string | null;
		} => {
			const isString = seqDataType === 'string';
			// アドレスの刻み幅: 文字列型は占有ワード数、32ビット型は 2、他は 1。
			let step = SEQ_STEP2_TYPES.includes(seqDataType) ? 2 : 1;
			let stringLength: number | null = null;
			const none = { rows: [], step, stringLength, device: null };
			if (isString) {
				const len = numOrNull(seqStringLength);
				if (
					len === null ||
					!Number.isInteger(len) ||
					len < MIN_STRING_LENGTH ||
					len > MAX_STRING_LENGTH
				) {
					return {
						...none,
						error: `文字列長は ${MIN_STRING_LENGTH}〜${MAX_STRING_LENGTH} の整数で入力してください`
					};
				}
				stringLength = len;
				step = len;
			}
			if (seqStart.trim() === '') return { ...none, stringLength, step, error: null };
			const parsed = parseSlmpDevice(seqStart);
			if (!parsed) {
				return {
					...none,
					stringLength,
					step,
					error: '開始デバイスを解釈できません（SLMP 記法。例: D100 / M10 / X1A）'
				};
			}
			const { device, number } = parsed;
			// データ型とデバイス種別の整合（サーバー側プランナーと同じ規則を
			// 先出しで検証。最終判定はサーバー）。文字列型はワードデバイス扱い。
			if (device.access === 'bit' && seqDataType !== 'bit') {
				return {
					...none,
					stringLength,
					step,
					device,
					error: `${device.mnemonic} はビットデバイスのため、データ型は bit を選択してください`
				};
			}
			if (device.access === 'word' && seqDataType === 'bit') {
				return {
					...none,
					stringLength,
					step,
					device,
					error: `${device.mnemonic} はワードデバイスのため、データ型 bit は使用できません`
				};
			}
			const count = numOrNull(seqCount);
			if (count === null || !Number.isInteger(count) || count < 1 || count > SEQ_MAX_COUNT) {
				return {
					...none,
					stringLength,
					step,
					device,
					error: `件数は 1〜${SEQ_MAX_COUNT} の整数で入力してください`
				};
			}
			const last = number + (count - 1) * step;
			if (last > SLMP_MAX_DEVICE_NUMBER) {
				return {
					...none,
					stringLength,
					step,
					device,
					error: `最終デバイス番号が SLMP の上限（${formatSlmpDevice(device, SLMP_MAX_DEVICE_NUMBER)}）を超えます`
				};
			}
			const rows: SeqRow[] = [];
			for (let i = 0; i < count; i++) {
				const address = formatSlmpDevice(device, number + i * step);
				// 名前はアドレス文字列そのものを自動割り付け（デバイス名 = 名称）。
				rows.push({ index: i, name: address, address, duplicate: existingTagNames.has(address) });
			}
			return { rows, step, stringLength, device, error: null };
		}
	);

	const seqDuplicateCount = $derived(seqPlan.rows.filter((r) => r.duplicate).length);
	/** 登録済み（ok）を除いた、今回の「登録」で実際に送信される行。 */
	const seqPendingRows = $derived(seqPlan.rows.filter((r) => !seqRowStatus[r.index]?.ok));

	/**
	 * プレビューの表示行: 件数が多いときは 先頭10件 + 「… 他N件」 + 最終1件
	 * に省略する。ただし登録失敗した行はエラーメッセージを確認できるよう
	 * 省略対象から外して必ず表示する。
	 */
	const seqDisplay = $derived.by((): { rows: SeqRow[]; hiddenCount: number } => {
		const rows = seqPlan.rows;
		if (rows.length <= 20) return { rows, hiddenCount: 0 };
		const shown = rows.filter(
			(r, i) => i < 10 || i === rows.length - 1 || seqRowStatus[r.index]?.ok === false
		);
		return { rows: shown, hiddenCount: rows.length - shown.length };
	});

	const seqSummary = $derived.by((): string | null => {
		const rows = seqPlan.rows;
		if (rows.length === 0) return null;
		return `${rows[0].address} 〜 ${rows[rows.length - 1].address}（${seqDataType}, step${seqPlan.step}, ${rows.length}件）を登録します`;
	});

	async function runSeqCreate(): Promise<void> {
		if (seqGroupId === '' || seqPlan.error !== null || seqPendingRows.length === 0) return;
		const collectionGroupId = Number(seqGroupId);
		const dataType = seqDataType;
		const stringLength = seqPlan.stringLength;
		const unit = seqUnit.trim() === '' ? null : seqUnit;
		const decimals = numOrNull(seqDecimals) ?? 0;
		const enabled = seqEnabled;
		seqRunning = true;
		try {
			const { okCount, failCount } = await runCreateLoop(
				seqPendingRows,
				(row) => row.index,
				(row) => ({
					name: row.name,
					collectionGroupId,
					address: row.address,
					dataType,
					stringLength,
					unit,
					decimals,
					enabled
				}),
				seqRowStatus,
				(status) => (seqRowStatus = status)
			);
			toastStore.push(
				failCount === 0 ? 'success' : 'error',
				`連続登録: 成功${okCount}件・失敗${failCount}件`
			);
			await reload();
			// 全行成功なら閉じる。失敗行はメッセージ付きでプレビューに残り、
			// 再実行では未登録の行だけが送信される（登録済みはスキップ）。
			if (failCount === 0) closeSeqModal();
		} finally {
			seqRunning = false;
		}
	}

	// --- modal escape handling (CommandPalette と同じく Esc で閉じる) ---
	function handleWindowKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Escape') return;
		if (seqModalOpen) {
			if (!seqRunning) closeSeqModal();
			event.preventDefault();
		} else if (bulkModalOpen) {
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

	// --- row delete + checkbox multi-select (feature/easy-delete) -----------
	//
	// BantoGrid はボタン／チェックボックスのセルを持たない（`cell` はテキスト
	// リンクのみ）ため、選択列（☑/☐）と「削除」列は format のテキストセルと
	// して描画し、ラッパー要素の capture フェーズでクリックを先取りする
	// （stopPropagation でセル自身の onclick = onRowClick（編集モーダル）を
	// 抑止）。セル DOM の data-cell-field / data-cell-row 属性で判定し、表示
	// 行 index はグリッドと同じ filterRows→sortRows パイプラインで実データへ
	// 写像する（BantoGrid.svelte クライアントモードと同式。groupBy 未使用）。
	// ヘッダーはボタン化できないため「全選択（表示中）」はツールバーに置く。

	/** 選択中のタグ id（SvelteSet: add/delete のミューテーションが通知される）。 */
	const selectedIds = new SvelteSet<number>();

	// 選択・削除列を出すのは編集者以上のみ。role は (app) レイアウトの load()
	// が解決してからページがマウントされるため、初期化時点で確定している
	// （session.svelte.ts の Ordering note 参照）。
	const showRowActions = canWriteResources(sessionStore.role);

	/** 行ごとの削除ボタン（「削除」列）: 軽い confirm 1回で1タグ削除。 */
	async function deleteTagRow(t: Tag): Promise<void> {
		if (!window.confirm(`タグ ${t.name} を削除しますか？`)) return;
		try {
			await deleteTag(t.id);
			toastStore.push('success', '削除しました');
			selectedIds.delete(t.id);
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	let selectedDeleting = $state(false);

	/**
	 * 選択削除: confirm 1回 → 既存の deleteTag を1件ずつ呼ぶ（各件が個別に
	 * 監査される）。失敗行があっても残りを続行し、成功／失敗件数をトーストで
	 * まとめる（runCreateLoop と同運用の削除版）。
	 */
	async function runSelectedDelete(): Promise<void> {
		const ids = [...selectedIds];
		if (ids.length === 0 || selectedDeleting) return;
		if (!window.confirm(`選択した ${ids.length} 件のタグを削除しますか？`)) return;
		selectedDeleting = true;
		let okCount = 0;
		let failCount = 0;
		let firstError = '';
		try {
			for (const id of ids) {
				try {
					await deleteTag(id);
					okCount++;
					selectedIds.delete(id);
				} catch (err) {
					failCount++;
					if (firstError === '') firstError = errorMessage(err);
				}
			}
			toastStore.push(
				failCount === 0 ? 'success' : 'error',
				`選択削除: 成功${okCount}件・失敗${failCount}件${firstError === '' ? '' : `（${firstError}）`}`
			);
			await reload();
		} finally {
			selectedDeleting = false;
		}
	}

	const columns: GridColumn<Tag>[] = [
		...(showRowActions
			? [
					{
						id: '_select',
						header: '選択',
						accessor: (t: Tag) => selectedIds.has(t.id),
						width: 60,
						align: 'center',
						sortable: false,
						resizable: false,
						format: (v: unknown) => (v ? '☑' : '☐')
					} satisfies GridColumn<Tag>
				]
			: []),
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
		{
			id: 'stringLength',
			header: '文字列長',
			accessor: (t) => (t.stringLength === null ? '—' : `${t.stringLength}語`),
			width: 80,
			align: 'right'
		},
		{ id: 'unit', header: '単位', accessor: 'unit', width: 80 },
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		},
		...(showRowActions
			? [
					{
						id: '_delete',
						header: '削除',
						accessor: () => '削除',
						width: 70,
						align: 'center',
						sortable: false,
						resizable: false
					} satisfies GridColumn<Tag>
				]
			: [])
	];

	const gridState = new GridState<Tag>(columns);
	/** グリッドの表示順（フィルタ＋ソート適用後）の行。capture ハンドラの
	 * data-cell-row 写像と「全選択（表示中）」の対象。 */
	const viewTags = $derived(
		sortRows(filterRows(tags, gridState.filters, columns), gridState.sort, columns)
	);
	const allViewSelected = $derived(
		viewTags.length > 0 && viewTags.every((t) => selectedIds.has(t.id))
	);

	/** ツールバーの「全選択（表示中）」/「全解除」トグル。 */
	function toggleSelectAllInView(): void {
		if (allViewSelected) {
			for (const t of viewTags) selectedIds.delete(t.id);
		} else {
			for (const t of viewTags) selectedIds.add(t.id);
		}
	}

	function handleGridClickCapture(event: MouseEvent): void {
		const target = event.target instanceof HTMLElement ? event.target : null;
		const cell = target?.closest<HTMLElement>(
			'[data-cell-field="_select"], [data-cell-field="_delete"]'
		);
		if (!cell || cell.dataset.cellRow === undefined) return;
		event.stopPropagation();
		const row = viewTags[Number(cell.dataset.cellRow)];
		if (!row) return;
		if (cell.dataset.cellField === '_select') {
			if (selectedIds.has(row.id)) selectedIds.delete(row.id);
			else selectedIds.add(row.id);
		} else {
			void deleteTagRow(row);
		}
	}
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
					<option value={dt}>{dt === 'string' ? '文字列(string)' : dt}</option>
				{/each}
			</select>
			{#if errors.dataType}<span class="err">{errors.dataType}</span>{/if}
		</label>
		{#if isStringType(form.dataType)}
			<label class="field">
				文字列長（{MIN_STRING_LENGTH}〜{MAX_STRING_LENGTH}ワード）
				<input
					type="number"
					min={MIN_STRING_LENGTH}
					max={MAX_STRING_LENGTH}
					bind:value={form.stringLength}
				/>
				<span class="hint">1ワード = 2バイト（Shift-JIS）。</span>
				{#if errors.stringLength}<span class="err">{errors.stringLength}</span>{/if}
			</label>
		{/if}
		<label class="field">
			単位
			<input type="text" bind:value={form.unit} />
		</label>
		<label class="field">
			小数桁
			<input type="number" min="0" max="6" bind:value={form.decimals} />
			{#if errors.decimals}<span class="err">{errors.decimals}</span>{/if}
		</label>
		{#if !isStringType(form.dataType)}
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
		{/if}
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
	</div>
	{#if isStringType(form.dataType)}
		<p class="note">文字列型ではスケーリング・しきい値は設定できません（SJIS テキストのため）。</p>
	{:else}
		<p class="note">
			スケーリング（raw/eng の上下限）は 4 つすべて入力するか、すべて空にしてください （空 =
			スケーリングなし）。しきい値は LL ≤ L ≤ H ≤ HH の順（設定した項目のみ比較）。
		</p>
	{/if}
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
				<button type="button" class="ghost" onclick={openSeqModal}>連続登録</button>
			{/if}
			<button type="button" class="ghost" onclick={openGroupModal}>収集グループ管理</button>
			{#if canWrite && tags.length > 0}
				<button type="button" class="ghost" onclick={toggleSelectAllInView}>
					{allViewSelected ? '全解除' : '全選択（表示中）'}
				</button>
			{/if}
			{#if canWrite && selectedIds.size > 0}
				<button
					type="button"
					class="danger"
					onclick={runSelectedDelete}
					disabled={selectedDeleting}
				>
					{selectedDeleting ? '削除中…' : `選択削除（${selectedIds.size}件）`}
				</button>
			{/if}
			<span class="toolbar-note">
				{canWrite
					? '行をクリックすると編集ポップアップが開きます。「選択」列で複数選択、「削除」列で行ごとに削除できます。'
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
				<!-- capture で「選択」「削除」列のクリックを onRowClick（編集
				     モーダル）より先に拾う（script 側 handleGridClickCapture
				     参照）。キーボード操作はグリッド本体が担うため、この
				     ラッパー自体は素通し。 -->
				<div class="grid-wrap" onclickcapture={handleGridClickCapture}>
					<BantoGrid
						rows={tags}
						{columns}
						state={gridState}
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
				<strong>名前, アドレス, データ型, 単位, 小数桁, 文字列長</strong>
				（名前・アドレス・データ型は必須。単位は空欄可、小数桁は空欄 = 0。 文字列長は string 型のとき
				6 列目に必須、数値型では空欄）。 先頭行のデータ型セルが有効値でない場合はヘッダー行として読み飛ばします。
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
					placeholder={BULK_PLACEHOLDER}></textarea>
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
								<th>文字列長</th>
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
									<td>{row.stringLengthRaw === '' ? '—' : row.stringLengthRaw}</td>
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

{#if seqModalOpen}
	<div class="overlay">
		<div class="modal" role="dialog" aria-modal="true" aria-label="連続登録" use:focusFirstField>
			<div class="modal-head">
				<h3>連続登録</h3>
				<button
					type="button"
					class="close"
					aria-label="閉じる"
					onclick={closeSeqModal}
					disabled={seqRunning}>×</button
				>
			</div>
			<p class="note">
				開始デバイスから連番でタグをまとめて登録します。<strong
					>名前はデバイス名（アドレス文字列）で自動割り付け</strong
				>されます（例: D100）。単位・小数桁・有効は全行に適用されます。
			</p>
			<div class="form-grid">
				<label class="field">
					収集グループ（全行に適用）
					<select bind:value={seqGroupId} disabled={seqRunning}>
						<option value="">選択してください</option>
						{#each groups as g (g.id)}
							<option value={String(g.id)}>{groupLabel(g)}</option>
						{/each}
					</select>
				</label>
				<label class="field">
					開始デバイス
					<input
						type="text"
						bind:value={seqStart}
						oninput={resetSeqStatus}
						placeholder="D100"
						disabled={seqRunning}
					/>
					<span class="hint">SLMP 記法（例: D100 / M10 / X1A）。X/Y/B/W/SB/SW は16進表記</span>
				</label>
				<label class="field">
					件数
					<input
						type="number"
						min="1"
						max={SEQ_MAX_COUNT}
						bind:value={seqCount}
						oninput={resetSeqStatus}
						disabled={seqRunning}
					/>
					<span class="hint"
						>最大 {SEQ_MAX_COUNT} 件（誤操作で一度に大量登録し過ぎないための上限）</span
					>
				</label>
				<label class="field">
					データ型（全行に適用）
					<select bind:value={seqDataType} onchange={resetSeqStatus} disabled={seqRunning}>
						{#each dataTypeOptions as dt (dt)}
							<option value={dt}>{dt === 'string' ? '文字列(string)' : dt}</option>
						{/each}
					</select>
				</label>
				{#if seqDataType === 'string'}
					<label class="field">
						文字列長（{MIN_STRING_LENGTH}〜{MAX_STRING_LENGTH}ワード）
						<input
							type="number"
							min={MIN_STRING_LENGTH}
							max={MAX_STRING_LENGTH}
							bind:value={seqStringLength}
							oninput={resetSeqStatus}
							disabled={seqRunning}
						/>
						<span class="hint">占有ワード数ぶんアドレスを刻んで生成します。</span>
					</label>
				{/if}
				<label class="field">
					単位
					<input type="text" bind:value={seqUnit} disabled={seqRunning} />
				</label>
				<label class="field">
					小数桁
					<input type="number" min="0" max="6" bind:value={seqDecimals} disabled={seqRunning} />
				</label>
				<label class="field checkbox">
					<input type="checkbox" bind:checked={seqEnabled} disabled={seqRunning} />
					有効
				</label>
			</div>
			{#if seqPlan.error !== null}
				<p class="err">{seqPlan.error}</p>
			{:else if seqPlan.rows.length > 0}
				<p class="bulk-summary">{seqSummary}</p>
				{#if seqDataType === 'string'}
					<p class="note">
						文字列型は {seqPlan.stringLength} ワードを占有するため、{seqPlan.step}番地刻みで生成します。
					</p>
				{:else if seqPlan.step === 2}
					<p class="note">32bit型（i32/u32/f32）は2ワードを占有するため、2番地刻みで生成します。</p>
				{/if}
				{#if seqDuplicateCount > 0}
					<p class="err">
						既存タグと同名の行が{seqDuplicateCount}件あります（該当行はサーバー側で重複エラーになります）。
					</p>
				{/if}
				<div class="bulk-preview-wrap">
					<table class="bulk-preview">
						<thead>
							<tr>
								<th>#</th>
								<th>名前</th>
								<th>アドレス</th>
								<th>状態</th>
							</tr>
						</thead>
						<tbody>
							{#each seqDisplay.rows as row, i (row.index)}
								{@const result = seqRowStatus[row.index]}
								{#if seqDisplay.hiddenCount > 0 && i === seqDisplay.rows.length - 1}
									<tr>
										<td class="num">…</td>
										<td colspan="3">他{seqDisplay.hiddenCount}件</td>
									</tr>
								{/if}
								<tr class:invalid={result?.ok === false}>
									<td class="num">{row.index + 1}</td>
									<td>{row.name}</td>
									<td>{row.address}</td>
									<td>
										{#if result}
											<span class={result.ok ? 'ok' : 'err'}>{result.message}</span>
										{:else if row.duplicate}
											<span class="err">既存タグと同名</span>
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
					onclick={runSeqCreate}
					disabled={seqRunning ||
						seqGroupId === '' ||
						seqPlan.error !== null ||
						seqPendingRows.length === 0}
				>
					{seqRunning ? '登録中…' : `登録（${seqPendingRows.length}件）`}
				</button>
				<button type="button" class="ghost" onclick={closeSeqModal} disabled={seqRunning}>
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

	/* 「選択」（☑/☐）と「削除」列（テキストセル）をボタン風に見せる。
	   BantoGrid のセル DOM（data-cell-field 属性）に :global で当てる。 */
	.grid-wrap :global([data-cell-field='_select']) {
		cursor: pointer;
		user-select: none;
		font-size: 1rem;
	}

	.grid-wrap :global([data-cell-field='_delete']) {
		color: var(--banto-danger);
		font-weight: 600;
		cursor: pointer;
		user-select: none;
	}

	.grid-wrap :global([data-cell-field='_delete']:hover) {
		text-decoration: underline;
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
