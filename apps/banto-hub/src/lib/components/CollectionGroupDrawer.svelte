<script lang="ts">
	/**
	 * T18-6b（TAG-UX-7/TAG-UX-8、2026-08-27 オーナー決定「収集グループの作成／
	 * 再設定を Drawer に寄せる」）: 収集グループの作成・再設定を担う共通部品。
	 * T18-6a の `ConnectionDrawer.svelte` を反復した実装（実装指示「同じ構造・
	 * 同じ流儀を踏襲すること」）— `Drawer.svelte`（汎用スライドオーバー、
	 * T13-1）を内包した自己完結コンポーネントにしてあるので、呼び出し側は
	 * `Drawer` を別途インスタンス化せず
	 * `<CollectionGroupDrawer open={...} group={...} .../>` を並べるだけでよい。
	 * `collection-groups/+page.svelte` 単独ページと、将来のタグツリー右クリック
	 * （T18-6d、別スライスが配線予定）の双方から使うことを狙って
	 * `src/lib/components/` に置く。
	 *
	 * **新規作成 = ウィザード（3ステップ）、再設定 = 単一フォーム**
	 * （`ConnectionDrawer.svelte` と同じ考え方。`group` prop が `null` かどうか
	 * で分岐する）。ステップは既存フィールドを自然に分けただけで、フィールド・
	 * 検証は旧 `collection-groups/+page.svelte` のページ内実装（BantoGrid一覧+
	 * 縦積み編集パネル構成）から1つも落としていない。PLC接続側と異なり
	 * 「接続テスト」に相当する工程は無い（収集グループは PLC 疎通確認の対象
	 * ではない）ので、3ステップ目は確認表示のみ:
	 * 1. 識別: 名前（TAG-UX-8 の連番プリフィル対象）
	 * 2. 接続先と周期: 所属する PLC 接続 / 収集周期 / 有効
	 * 3. 確認: 入力内容の確認表示 + 「作成」
	 *
	 * **T19 S1-b（UX-31、2026-09-02 オーナー決定「作成＝中央モーダル、
	 * 編集＝右ペイン」）: 作成ウィザードは `Modal.svelte`（中央）、再設定
	 * フォームと閲覧専用モードは `Drawer.svelte`（右ペイン）で描画する**
	 * （`ConnectionDrawer.svelte` と同じ分岐 - `isCreate` で提示先自体を
	 * 切り替える。`isCreate` は `group` prop の null 性から決まり、Drawer/
	 * Modal を開いている間に値が変わることはない前提）。
	 *
	 * 純関数部分（連番採番・フォーム⇄API入力変換）は
	 * `$lib/banto/collectionGroupForm.ts` へ切り出し済み（そちらでユニット
	 * テスト済み）。採番ロジック自体は `plcConnectionForm.ts::nextConnectionName`
	 * と共通の `sequentialName.ts::nextSequentialName` を使う。
	 *
	 * **連番プリフィルは pending queue も見る**（実機で再現した不具合の修正1、
	 * 2026-08-31 オーナー報告）: 収集稼働中の作成は 202 でキューに入るだけで
	 * DB（`existingNames`）には現れないため、既存レコードだけを見る連番採番
	 * では稼働中に Drawer を複数回開くたびに同じ名前が提案され、後から
	 * 一括適用すると名前の一意制約で全滅する（オーナーが実機で再現: 収集
	 * 稼働中に3回作成 → 3回とも `group1` が提案され全部衝突）。開いた直後は
	 * 従来どおり `existingNames` だけで即座に仮の名前を出しつつ、裏で
	 * `listPendingChanges()`（admin 限定 API）を取得して pending 内の
	 * 未適用の `collection_groups.create` 分（`pendingCreateNames.ts`）も
	 * 候補に加え直す。ユーザーが名前欄を編集する前に取得が終われば
	 * 差し替える（`provisionalName`）。**pending の取得に失敗（権限不足含む）
	 * しても既存レコードだけでの採番のまま続行する**（プリフィルは利便性
	 * 機能であり、これで作成自体を止めない）。
	 *
	 * **202 (QueuedWhileRunningError, 収集稼働中のキュー投入) の扱い**
	 * （実装指示6）: `ConnectionDrawer.svelte` と同じく失敗ではなく案内として
	 * `toastStore.push('info', ...)` を使う。Drawer は閉じずフォームを保持する。
	 *
	 * **所属する PLC 接続の既定値**（実装指示4）: `presetPlcConnectionId` prop
	 * を通じて、Drawer を開いた文脈（ページの `?connectionId=` クエリ、または
	 * 将来 T18-6d のツリー接続ノードからの起動）を尊重する。新規作成フォームを
	 * 開いた時点でこの prop が非 `null` ならその接続をプリセットし、`null`
	 * （既定 `undefined`）ならページの旧来どおり未選択のまま出す。
	 */
	import { isProviderError } from '@banto/admin-core';
	import Drawer from './Drawer.svelte';
	import Modal from './Modal.svelte';
	import { toastStore } from '$lib/toast.svelte';
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';
	import { listPendingChanges, type PendingChange } from '$lib/banto/pendingChangesAdmin';
	import { pendingCreateNames } from '$lib/banto/pendingCreateNames';
	import {
		createCollectionGroup,
		deleteCollectionGroup,
		isQueuedWhileRunningError,
		updateCollectionGroup,
		ALLOWED_PERIOD_MS,
		type CollectionGroup,
		type PlcConnection
	} from '$lib/banto/tagRegistryAdmin';
	import {
		blankGroupForm,
		DEFAULT_PERIOD_MS,
		formToGroupInput,
		groupToForm,
		nextGroupName,
		type CollectionGroupFormState
	} from '$lib/banto/collectionGroupForm';
	import {
		getGroupDefaultWritable,
		setGroupDefaultWritable
	} from '$lib/banto/groupWritableDefault';

	/** `pendingChangesAdmin.ts::PendingChange.source` - `rest.rs::collection_groups_create` が `queue_pending_registry_change` に渡す文字列と一致させる。 */
	const PENDING_SOURCE = 'collection_groups.create';

	interface Props {
		open: boolean;
		/** `null` なら新規作成（ウィザード）。非 `null` ならそのグループの再設定（単一フォーム）。 */
		group: CollectionGroup | null;
		/** TAG-UX-8 の連番プリフィルに使う、既存のグループ名一覧（新規作成時のみ参照）。 */
		existingNames: string[];
		/** PLC接続の選択肢（新規作成・再設定共通、一覧ページから取得済みのものを渡す）。 */
		connections: PlcConnection[];
		/**
		 * 新規作成フォームを開いたときに所属PLC接続としてプリセットする ID
		 * （実装指示4「Drawer を開いた文脈を尊重する」- T18-6d でツリーの接続
		 * ノードから開く際に使う想定。`collection-groups/+page.svelte` は現行の
		 * `?connectionId=` クエリから解決した値をここへ渡す）。`null`/未指定
		 * なら従来どおり未選択のまま出す。
		 */
		presetPlcConnectionId?: number | null;
		/**
		 * T18-6d（タグツリー右クリック「収集グループを削除」からの起動）:
		 * `true` かつ `group` が非 `null`（再設定モード）で Drawer を開いた
		 * 直後、フォーム初期化に続けて既存の `handleDelete` をそのまま1回だけ
		 * 呼ぶ - 確認ダイアログ（`window.confirm`）・削除影響エラー（タグが
		 * 参照している場合の Validation エラー）の扱いはすべて `handleDelete`
		 * の実装をそのまま流用し、ここでは独自の削除処理を持たない
		 * （`ConnectionDrawer.svelte` の同名 prop と同じ考え方）。確認を
		 * キャンセルした場合はこの Drawer に留まり、そのまま再設定フォームと
		 * して使い続けられる。既定 `false`。
		 */
		requestDelete?: boolean;
		/**
		 * T19 S1-a（docs/banto-hub-t19-design.md §7.1「viewer ロールからの
		 * 接続・グループ詳細の閲覧」）: `ConnectionDrawer.svelte` の同名
		 * prop と同じ考え方 - `true` なら閲覧専用モードで開く（入力は
		 * すべて `disabled`、保存・削除のボタンは出さない）。常に既存の
		 * グループ（`group` 非 `null`）と組み合わせて使う想定。既定 `false`。
		 */
		readOnly?: boolean;
		onClose: () => void;
		/** 作成/更新が成功した直後に呼ばれる（202キュー投入時は呼ばれない — まだ確定していないため）。 */
		onSaved: (group: CollectionGroup) => void;
		/** 削除が成功した直後に呼ばれる。 */
		onDeleted: (id: number) => void;
	}

	let {
		open,
		group,
		existingNames,
		connections,
		presetPlcConnectionId = null,
		requestDelete = false,
		readOnly = false,
		onClose,
		onSaved,
		onDeleted
	}: Props = $props();

	const isCreate = $derived(group === null);
	const drawerTitle = $derived(
		isCreate ? '新規作成' : readOnly ? `${group?.name} の詳細` : `${group?.name} を編集`
	);

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

	let form: CollectionGroupFormState = $state(blankGroupForm(DEFAULT_PERIOD_MS));
	let errors: Record<string, string> = $state({});
	let saving = $state(false);
	let deleting = $state(false);
	let step: 1 | 2 | 3 = $state(1);

	/**
	 * T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2「収集グループ単位で
	 * 既定値を変更できるようにします」、2026-09-02 オーナー決定）: 「この
	 * グループの新規タグは既定で書込可」チェックボックスの現在値。
	 *
	 * **サーバーの `CollectionGroup`/`CollectionGroupInput` の一部ではない**
	 * - `$lib/banto/groupWritableDefault.ts` の doc comment に書いた理由
	 * （`banto-tags` が relay-wright/banto-collect とも共有するクレートで、
	 * DB 列化の影響範囲がそれら無関係なアプリに及ぶと判明したため）で
	 * `localStorage` 側に持つ。新規作成では対象グループの id がまだ無い
	 * ため、作成成功後（`handleCreate` の `created.id` 確定後）に初めて
	 * 保存する - それまではこの `$state` だけがユーザーの選択を保持する。
	 */
	let defaultWritablePref = $state(true);

	/**
	 * Drawer を開いた対象（新規作成 or どのグループの再設定か）を表すキー。
	 * `ConnectionDrawer.svelte::lastOpenKey` と同じ役割・同じ理由
	 * （保存成功後に親が `groups` を再取得して新しい `CollectionGroup`
	 * オブジェクト（同じ id）を渡してきても、このキーは変わらないため未保存
	 * 編集を巻き戻さない - `handleSave` 側が保存直後に明示的に `form` を
	 * 正規化済みの値へ差し替える）。
	 */
	let lastOpenKey: string | null = null;

	/**
	 * 修正1（実機で再現した不具合、2026-08-31 オーナー報告）: 新規作成フォームを
	 * 開いた直後に `nextGroupName(existingNames)` だけで即座に割り当てた
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
		const key = group ? `edit:${group.id}` : 'create';
		if (key === lastOpenKey) return;
		lastOpenKey = key;

		if (group) {
			form = groupToForm(group);
			provisionalName = null;
			// T19 S1-b（UX-34）: 既存グループの再設定 - このブラウザに保存済みの
			// 既定値を読み直す（未設定なら `getGroupDefaultWritable` 自体が
			// 全体既定の `true` を返す）。
			defaultWritablePref = getGroupDefaultWritable(group.id);
		} else {
			// T19 S1-b（UX-34）: 新規作成は id が無いのでこのブラウザの保存値を
			// 参照しようがない - 全体既定の `true` から始める。
			defaultWritablePref = true;
			const blank = blankGroupForm(DEFAULT_PERIOD_MS);
			const initialName = nextGroupName(existingNames);
			blank.name = initialName;
			provisionalName = initialName;
			if (presetPlcConnectionId != null) blank.plcConnectionId = String(presetPlcConnectionId);
			form = blank;
			void refinePendingNamePrefill(key);
		}
		errors = {};
		step = 1;

		// T18-6d: 「収集グループを削除」からの起動 - フォーム初期化直後に
		// 既存の handleDelete を1回だけ呼ぶ（上の Props.requestDelete 参照）。
		// readOnly では呼び出し側が requestDelete を渡すことは無い想定だが、
		// 念のため二重に閲覧専用を守る。
		if (requestDelete && group && !readOnly) {
			void handleDelete();
		}
	});

	/**
	 * pending queue（`GET /api/pending-changes`、admin 限定）を取得し、
	 * まだ適用されていない `collection_groups.create` の名前も連番プリフィル
	 * の衝突候補に加えて `form.name` を差し替える。上の `$effect` からの
	 * fire-and-forget 呼び出し専用（本体はモジュール doc comment の
	 * 「連番プリフィルは pending queue も見る」参照）。
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
		const refined = nextGroupName(existingNames, 'group', pendingNames);
		if (form.name === provisionalName) form.name = refined;
		provisionalName = refined;
	}

	/** 送信前のフィールド → 該当ウィザードステップの対応（作成時のエラー誘導用）。 */
	const FIELD_STEP: Record<string, 1 | 2 | 3> = {
		name: 1,
		plcConnectionId: 2,
		periodMs: 2
	};

	function stepForFieldErrors(fieldErrors: Record<string, string>): 1 | 2 | 3 | null {
		let target: 1 | 2 | 3 | null = null;
		for (const field of Object.keys(fieldErrors)) {
			const s = FIELD_STEP[field];
			if (s !== undefined && (target === null || s < target)) target = s;
		}
		return target;
	}

	const canAdvanceFromStep1 = $derived(form.name.trim() !== '');
	const canAdvanceFromStep2 = $derived(form.plcConnectionId !== '');

	function goNext(): void {
		if (step === 1 && canAdvanceFromStep1) step = 2;
		else if (step === 2 && canAdvanceFromStep2) step = 3;
	}

	function goBack(): void {
		if (step > 1) step = (step - 1) as 1 | 2 | 3;
	}

	function connectionName(id: string): string {
		const numId = Number(id);
		return connections.find((c) => c.id === numId)?.name ?? '（未選択）';
	}

	async function handleCreate(): Promise<void> {
		saving = true;
		errors = {};
		try {
			const created = await createCollectionGroup(formToGroupInput(form));
			toastStore.push('success', '作成しました');
			// T19 S1-b（UX-34）: 作成が確定した時点で初めて id が確定するため、
			// ここで初めて（このブラウザの）既定値を保存する。202 キュー投入時
			// はこの then 節に来ない（下の catch の `isQueuedWhileRunningError`
			// 分岐 - まだ id が確定していないため保存しようがなく、それでよい:
			// 後で実際にこのグループを再設定 Drawer で開いたときにまた選べる）。
			setGroupDefaultWritable(created.id, defaultWritablePref);
			onSaved(created);
			onClose();
		} catch (err) {
			if (isQueuedWhileRunningError(err)) {
				// 実装指示6: 失敗ではなく案内として扱う（Drawerは開いたまま）。
				toastStore.push('info', err.message);
				return;
			}
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) {
				errors = fieldErrors;
				const target = stepForFieldErrors(fieldErrors);
				if (target !== null) step = target;
			} else {
				toastStore.push('error', errorMessage(err));
			}
		} finally {
			saving = false;
		}
	}

	async function handleSave(): Promise<void> {
		if (!group) return;
		saving = true;
		errors = {};
		try {
			const updated = await updateCollectionGroup(group.id, formToGroupInput(form));
			toastStore.push('success', '更新しました');
			// 保存成功後はサーバーの正規化値を基準に取り直す（ConnectionDrawer
			// の handleSave と同じ方針）。Drawer は閉じない。
			form = groupToForm(updated);
			// T19 S1-b（UX-34）: 202 キュー投入時（下の catch）はまだ確定して
			// いないため保存しない - 実際に適用されてから再度開いたときの
			// 選択に委ねる。
			setGroupDefaultWritable(updated.id, defaultWritablePref);
			onSaved(updated);
		} catch (err) {
			if (isQueuedWhileRunningError(err)) {
				toastStore.push('info', err.message);
				return;
			}
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) errors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	async function handleDelete(): Promise<void> {
		if (!group) return;
		if (!window.confirm(`${group.name} を削除しますか？`)) return;
		deleting = true;
		try {
			await deleteCollectionGroup(group.id);
			toastStore.push('success', '削除しました');
			onDeleted(group.id);
			onClose();
		} catch (err) {
			if (isQueuedWhileRunningError(err)) {
				toastStore.push('info', err.message);
				return;
			}
			// タグが参照している場合はサービス層の分かりやすい Validation
			// エラー（件数入り）がここに来る。
			toastStore.push('error', errorMessage(err));
		} finally {
			deleting = false;
		}
	}

	function isBusy(): boolean {
		return saving || deleting;
	}

	/** 処理中は ×・Esc・オーバーレイクリックでの close を抑止する。 */
	function onRequestClose(): boolean {
		return !isBusy();
	}
</script>

{#snippet nameField()}
	<label class="field">
		名前
		<input type="text" id="group-name" bind:value={form.name} disabled={readOnly} />
		{#if errors.name}<span class="err">{errors.name}</span>{/if}
	</label>
{/snippet}

{#snippet destinationFields()}
	<div class="form-grid">
		<label class="field">
			PLC接続
			<select bind:value={form.plcConnectionId} disabled={readOnly}>
				<option value="" disabled>選択してください</option>
				{#each connections as conn (conn.id)}
					<option value={String(conn.id)}>{conn.name}</option>
				{/each}
			</select>
			{#if errors.plcConnectionId}<span class="err">{errors.plcConnectionId}</span>{/if}
		</label>
		<label class="field">
			収集周期
			<select bind:value={form.periodMs} disabled={readOnly}>
				{#each ALLOWED_PERIOD_MS as ms (ms)}
					<option value={String(ms)}>{ms} ms</option>
				{/each}
			</select>
			{#if errors.periodMs}<span class="err">{errors.periodMs}</span>{/if}
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} disabled={readOnly} />
			有効
		</label>
		<!--
			T19 S1-b（UX-34「収集グループ単位で既定値を変更できるようにする」、
			2026-09-02 オーナー決定）: このグループへの新規タグ登録が
			「書き込み可（writable）」チェックボックスをどちらの状態で
			始めるかを、グループ単位で選べる。サーバーの `CollectionGroup`
			には保存しない（このブラウザの `localStorage` にのみ残る -
			`$lib/banto/groupWritableDefault.ts` の doc comment に理由あり）。
			`writable` の実際の登録可否（computed タグ拒否、および
			Modbus 読み取り専用領域拒否を含む8段ゲート - 後者は S1-b0 で
			UI に配線予定、`$lib/banto/writableDefault.ts` 参照）には
			一切影響しない - あくまで新規タグフォームを開いた瞬間の
			チェックボックスの初期値だけを決める。
		-->
		<label class="field checkbox wide">
			<input type="checkbox" bind:checked={defaultWritablePref} disabled={readOnly} />
			このグループの新規タグは既定で書込可（writable）
		</label>
		<span class="hint wide">
			チェックを入れると、このグループへ新規タグを登録するとき「外部クライアントから PLC
			への書き込みを許可」が既定でオンになります（computed タグには適用されません）。個々の
			タグ側でいつでも上書きできます。
		</span>
	</div>
{/snippet}

{#snippet confirmSummary()}
	<dl class="summary">
		<dt>名前</dt>
		<dd>{form.name || '（未入力）'}</dd>
		<dt>PLC接続</dt>
		<dd>{form.plcConnectionId ? connectionName(form.plcConnectionId) : '（未選択）'}</dd>
		<dt>収集周期</dt>
		<dd>{form.periodMs} ms</dd>
		<dt>有効</dt>
		<dd>{form.enabled ? 'はい' : 'いいえ'}</dd>
		<dt>新規タグの書込可既定</dt>
		<dd>{defaultWritablePref ? 'ON' : 'OFF'}</dd>
	</dl>
{/snippet}

{#if isCreate}
	<Modal
		open={open && isCreate}
		title={drawerTitle}
		{onRequestClose}
		onclose={onClose}
		width="560px"
	>
		<ol class="wizard-steps" aria-label="作成手順">
			<li class:active={step === 1} class:done={step > 1}>1. 識別</li>
			<li class:active={step === 2} class:done={step > 2}>2. 接続先と周期</li>
			<li class:active={step === 3}>3. 確認</li>
		</ol>

		{#if step === 1}
			{@render nameField()}
		{:else if step === 2}
			{@render destinationFields()}
		{:else}
			{@render confirmSummary()}
		{/if}

		<div class="wizard-actions">
			{#if step > 1}
				<button type="button" class="secondary" onclick={goBack} disabled={saving}>戻る</button>
			{/if}
			{#if step === 1}
				<button type="button" onclick={goNext} disabled={!canAdvanceFromStep1}>次へ</button>
			{:else if step === 2}
				<button type="button" onclick={goNext} disabled={!canAdvanceFromStep2}>次へ</button>
			{:else}
				<button type="button" onclick={handleCreate} disabled={saving}>作成</button>
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
	>
		{@render nameField()}
		{@render destinationFields()}
		{#if !readOnly}
			<div class="actions">
				<button type="button" onclick={handleSave} disabled={saving || deleting}>保存</button>
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

	/* T19 S1-b（UX-34）: 「このグループの新規タグは既定で書込可」チェックボックスと補足文を全幅にする。 */
	.field.wide {
		grid-column: 1 / -1;
	}

	.hint {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	.hint.wide {
		grid-column: 1 / -1;
		margin: 0;
	}

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
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
