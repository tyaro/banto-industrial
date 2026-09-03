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
	 * 2. プロトコルと接続先: プロトコル/ホスト/ポート/ユニットID/
	 *    （SLMPのみ）ワード順/有効/シミュレーション
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
	 * `$lib/banto/plcConnectionForm.ts` へ切り出し済み（そちらでユニット
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
		createPlcConnection,
		deletePlcConnection,
		isQueuedWhileRunningError,
		testPlcConnection,
		updatePlcConnection,
		WORD_ORDER_OPTIONS,
		type PlcConnection,
		type PlcConnectionTestResult
	} from '$lib/banto/tagRegistryAdmin';
	import {
		PROTOCOL_OPTIONS,
		blankConnectionForm,
		connectionToForm,
		defaultPortFor,
		formToConnectionInput,
		isDefaultPortForProtocol,
		nextConnectionName,
		type PlcConnectionFormState
	} from '$lib/banto/plcConnectionForm';
	import {
		countConnectionCascadeImpact,
		formatConnectionDeleteConfirmMessage
	} from '$lib/banto/registryCascadeImpact';
	import type { CollectionGroup, Tag } from '$lib/banto/tagRegistryAdmin';

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
		/** 削除が成功した直後に呼ばれる。 */
		onDeleted: (id: number) => void;
	}

	let {
		open,
		connection,
		existingNames,
		groups,
		tags,
		requestDelete = false,
		readOnly = false,
		onClose,
		onSaved,
		onDeleted
	}: Props = $props();

	const isCreate = $derived(connection === null);
	const drawerTitle = $derived(
		isCreate ? '新規作成' : readOnly ? `${connection?.name} の詳細` : `${connection?.name} を編集`
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

	let form: PlcConnectionFormState = $state(blankConnectionForm());
	let errors: Record<string, string> = $state({});
	let saving = $state(false);
	let deleting = $state(false);
	let testState: TestState = $state(blankTestState());
	let step: 1 | 2 | 3 = $state(1);
	/**
	 * 現在のポートがまだ「プロトコルの既定値のまま（未編集）」かどうか。
	 * 実装指示「プロトコルを切り替えたときポートが未編集（既定値のまま）
	 * なら追従させ、ユーザーが明示的に編集した後は勝手に上書きしないこと」
	 * を満たすための追跡フラグ - `false` の間だけ `onProtocolChange` が
	 * ポートを新プロトコルの既定値へ書き換える。ユーザーがポート欄を直接
	 * 編集した時点で `true` に固定する。
	 */
	let portTouched = $state(false);

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
		const key = connection ? `edit:${connection.id}` : 'create';
		if (key === lastOpenKey) return;
		lastOpenKey = key;

		if (connection) {
			form = connectionToForm(connection);
			provisionalName = null;
		} else {
			const blank = blankConnectionForm();
			const initialName = nextConnectionName(existingNames);
			blank.name = initialName;
			provisionalName = initialName;
			form = blank;
			void refinePendingNamePrefill(key);
		}
		errors = {};
		testState = blankTestState();
		step = 1;
		portTouched = !isDefaultPortForProtocol(form.port, form.protocol);

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
		if (form.name === provisionalName) form.name = refined;
		provisionalName = refined;
	}

	function onProtocolChange(): void {
		if (portTouched) return;
		const def = defaultPortFor(form.protocol);
		if (def !== undefined) form.port = String(def);
	}

	function onPortInput(): void {
		portTouched = true;
	}

	/** 送信前のフィールド → 該当ウィザードステップの対応（作成時のエラー誘導用）。 */
	const FIELD_STEP: Record<string, 1 | 2 | 3> = {
		name: 1,
		protocol: 2,
		host: 2,
		port: 2,
		unitId: 2,
		wordOrder: 2
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
	const canAdvanceFromStep2 = $derived(form.host.trim() !== '');

	function goNext(): void {
		if (step === 1 && canAdvanceFromStep1) step = 2;
		else if (step === 2 && canAdvanceFromStep2) step = 3;
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
		testState.testing = true;
		testState.result = null;
		try {
			testState.result = await testPlcConnection({
				protocol: form.protocol,
				host: form.host,
				port: Number(form.port),
				unitId: Number(form.unitId),
				simulation: form.simulation,
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

	async function handleCreate(): Promise<void> {
		saving = true;
		errors = {};
		try {
			const created = await createPlcConnection(formToConnectionInput(form));
			toastStore.push('success', '作成しました');
			onSaved(created);
			onClose();
		} catch (err) {
			if (isQueuedWhileRunningError(err)) {
				// 実装指示5: 失敗ではなく案内として扱う（Drawerは開いたまま）。
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
		if (!connection) return;
		saving = true;
		errors = {};
		try {
			const updated = await updatePlcConnection(connection.id, formToConnectionInput(form));
			toastStore.push('success', '更新しました');
			// 保存成功後はサーバーの正規化値を基準に取り直す（tags/+page.svelte
			// の saveEdit と同じ方針）。Drawer は閉じない。
			form = connectionToForm(updated);
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

	/**
	 * T19 S2-b（UX-38）: 確認メッセージに「消える定義の件数」と「履歴は残る」
	 * を明示する（実装指示「何件消えるか分からないまま削除させない」）。
	 * バックエンドはこの接続を消すと配下の収集グループ・タグを全部まとめて
	 * 削除する（`cascade_delete_tx` - もう拒否しない）ため、影響が見えない
	 * と事故になる。
	 */
	async function handleDelete(): Promise<void> {
		if (!connection) return;
		const impact = countConnectionCascadeImpact(connection.id, groups, tags);
		if (!window.confirm(formatConnectionDeleteConfirmMessage(connection.name, impact))) return;
		deleting = true;
		try {
			await deletePlcConnection(connection.id);
			toastStore.push('success', '削除しました');
			onDeleted(connection.id);
			onClose();
		} catch (err) {
			if (isQueuedWhileRunningError(err)) {
				toastStore.push('info', err.message);
				return;
			}
			// 収集グループが参照している場合はサービス層の分かりやすい
			// Validation エラー（件数入り）がここに来る。
			toastStore.push('error', errorMessage(err));
		} finally {
			deleting = false;
		}
	}

	function isBusy(): boolean {
		return saving || deleting || testState.testing;
	}

	/** 処理中は ×・Esc・オーバーレイクリックでの close を抑止する。 */
	function onRequestClose(): boolean {
		return !isBusy();
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
			<select bind:value={form.protocol} onchange={onProtocolChange} disabled={readOnly}>
				{#each PROTOCOL_OPTIONS as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.protocol}<span class="err">{errors.protocol}</span>{/if}
		</label>
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
				oninput={onPortInput}
				disabled={readOnly}
			/>
			{#if errors.port}<span class="err">{errors.port}</span>{/if}
		</label>
		<label class="field">
			ユニットID
			<input type="number" min="0" max="255" bind:value={form.unitId} disabled={readOnly} />
			<span class="hint">Modbus 用のスレーブID（0〜255）。SLMP では未使用（既定 1 のまま）。</span>
			{#if errors.unitId}<span class="err">{errors.unitId}</span>{/if}
		</label>
		{#if form.protocol === 'slmp'}
			<label class="field">
				ワード順
				<select bind:value={form.wordOrder} disabled={readOnly}>
					{#each WORD_ORDER_OPTIONS as opt (opt.value)}
						<option value={opt.value}>{opt.label}</option>
					{/each}
				</select>
				<span class="hint">
					32bit値（u32/f32等）の上位/下位ワードの並び。機種のマニュアルで確認してください —
					間違えると値が化けます（上位/下位が入れ替わります）。
				</span>
				{#if errors.wordOrder}<span class="err">{errors.wordOrder}</span>{/if}
			</label>
		{/if}
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} disabled={readOnly} />
			有効
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.simulation} disabled={readOnly} />
			シミュレーションモード
		</label>
		<span class="hint sim-hint">
			実PLCの代わりに内蔵シミュレータに接続します（開発・検証用）。本番運用では有効にしないでください。
		</span>
	</div>
{/snippet}

{#snippet testConnectionBlock()}
	<div class="test-connection">
		<button type="button" class="test-btn" onclick={runConnectionTest} disabled={testState.testing}>
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

{#snippet confirmSummary()}
	<dl class="summary">
		<dt>名前</dt>
		<dd>{form.name || '（未入力）'}</dd>
		<dt>プロトコル</dt>
		<dd>{PROTOCOL_OPTIONS.find((o) => o.value === form.protocol)?.label ?? form.protocol}</dd>
		<dt>ホスト</dt>
		<dd>{form.host || '（未入力）'}:{form.port}</dd>
		<dt>ユニットID</dt>
		<dd>{form.unitId}</dd>
		{#if form.protocol === 'slmp'}
			<dt>ワード順</dt>
			<dd>{WORD_ORDER_OPTIONS.find((o) => o.value === form.wordOrder)?.label ?? form.wordOrder}</dd>
		{/if}
		<dt>有効</dt>
		<dd>{form.enabled ? 'はい' : 'いいえ'}</dd>
		<dt>シミュレーション</dt>
		<dd>{form.simulation ? '⚠ シミュレーション中' : 'いいえ'}</dd>
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
			<li class:active={step === 2} class:done={step > 2}>2. プロトコルと接続先</li>
			<li class:active={step === 3}>3. 確認</li>
		</ol>

		{#if step === 1}
			{@render nameField()}
		{:else if step === 2}
			{@render destinationFields()}
		{:else}
			{@render confirmSummary()}
			{@render testConnectionBlock()}
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
			{@render testConnectionBlock()}
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

	.hint {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	.sim-hint {
		grid-column: 1 / -1;
		margin-top: -0.4rem;
		color: var(--banto-warning);
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
