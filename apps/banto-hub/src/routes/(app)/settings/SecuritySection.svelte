<script lang="ts">
	/**
	 * セキュリティカテゴリ（試運転モードのロックダウン、設計 §5.6・
	 * 2026-08-30 オーナー決定）（#359 段階1）。元 `+page.svelte` の該当
	 * セクションから markup・state・関数を無改変で移した。
	 *
	 * 表示条件は **サーバーが試運転モード（未ロックダウン）か**
	 * （`commissioningState.serverCommissioning`）の1つ（ロックダウン済みなら
	 * false なのでセクションごと非表示になる - 実装指示「ロックダウン済みのときは
	 * この操作を表示しないこと」）。セッションの種別では決めない: アカウントで
	 * ログインしたままでもロックダウンできる（操作の権限は admin、アカウントでも
	 * 試運転の grant でも可。2026-10-04 オーナー指示）。
	 *
	 * admin アカウントが1件も無いとサーバーが拒否する（詰み防止、
	 * `apps/banto-hub/core/src/commissioning.rs` の `no_admin_account_error`）。
	 * 実行してから失敗を見せるより「そもそも押せない」方が親切なので、
	 * `listUsers()`（既存の管理 API、バックエンド変更なしで流用できる）で
	 * 事前に admin の有無を確認し、無ければボタンを無効化して理由を出す。
	 * この事前チェックはあくまで UX 用のヒントで、権威ある判定は最終的に
	 * サーバー側の `lock_down()` が行う（`handleLockDown` の catch で
	 * validation エラーを表示するのはそのため - 事前チェックと実行の間に
	 * 他クライアントが最後の admin を消す、というレースも理論上あり得る）。
	 */
	import { goto, refreshAll } from '$app/navigation';
	import { toastStore } from '#lib/toast.svelte.js';
	import { resolveAppPath } from '#lib/navigation.js';
	import { commissioningState } from '#lib/banto/commissioningState.svelte.js';
	import { lockDown } from '#lib/banto/commissioning.js';
	import { lockDownAndLeave } from '#lib/banto/commissioningLockDown.js';
	import { listUsers } from '#lib/banto/usersAdmin.js';
	import { errorMessage } from './shared';

	const NO_ADMIN_MESSAGE =
		'管理者（adminロール）アカウントが1件も存在しないため、ロックダウンできません。' +
		'この状態で施錠すると誰もログインできなくなり、管理操作が一切できなくなります。' +
		'先にユーザー管理から管理者アカウントを作成してください。';

	/** null = 未確認（読み込み中 or 取得失敗）。安全側でボタンは無効のまま。 */
	let hasAdminAccount: boolean | null = $state(null);
	let adminCheckError: string | null = $state(null);
	let lockingDown = $state(false);
	let lockDownError: string | null = $state(null);

	$effect(() => {
		if (!commissioningState.serverCommissioning) return;
		let cancelled = false;
		(async () => {
			try {
				const users = await listUsers();
				if (!cancelled) hasAdminAccount = users.some((u) => u.role === 'admin');
			} catch (err) {
				if (!cancelled) adminCheckError = errorMessage(err);
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	const lockDownDisabledReason = $derived(
		lockingDown
			? null // ボタン自体は disabled になるが、理由表示は「読み込み中/不可」時のみでよい
			: hasAdminAccount === null
				? (adminCheckError ?? '管理者アカウントの有無を確認しています…')
				: hasAdminAccount === false
					? NO_ADMIN_MESSAGE
					: null
	);

	async function handleLockDown(): Promise<void> {
		if (
			!window.confirm(
				'ロックダウンを実行すると元に戻せません。' +
					'以後、管理操作にはログインが必須になります（試運転モードへは UI から戻せません）。' +
					'実行しますか？'
			)
		) {
			return;
		}

		lockingDown = true;
		lockDownError = null;
		try {
			// ロックダウン後は以後の全リクエストで認証が必須になる - この画面に
			// 留まらせると後続の管理 API 呼び出しが軒並み 401 になって壊れて
			// 見えるため、ログイン画面へ誘導する（実装指示のとおり）。
			// banto v3.0.0（ADR-0017）: 試運転の grant のセッションなら、ロックダウンの
			// 保存の直後にサーバーがそのトークンを全部失効させるので、確認 → 確定した
			// `none` なら /login、の順序は `commissioningLockDown.ts` の doc。アカウントで
			// ログインしたままなら本人のトークンは有効なままで 'stayed'。
			const outcome = await lockDownAndLeave({
				lockDown,
				goToLogin: () => goto(resolveAppPath('/login'))
			});
			if (outcome === 'left') {
				toastStore.push('success', 'ロックダウンしました。ログイン画面へ移動します。');
			} else if (outcome === 'stayed') {
				// 保存していたトークンが有効だった: そのアカウントで続ける。サーバーは
				// もうロックダウン済みなので、この欄（security カテゴリ）を消す:
				// 設定グループの load を走らせ直し、`guardCategory` が先頭の可視
				// カテゴリへ送る。
				toastStore.push('success', 'ロックダウンしました。ログイン中のアカウントで続けます。');
				commissioningState.markLockedDown();
				await refreshAll();
			} else {
				toastStore.push(
					'error',
					'ロックダウンしましたが、ログイン状態を確認できませんでした。しばらくしてから再試行してください。'
				);
			}
		} catch (err) {
			lockDownError = errorMessage(err);
		} finally {
			lockingDown = false;
		}
	}
</script>

{#if commissioningState.serverCommissioning}
	<section class="commissioning">
		<h2>試運転モードのロックダウン</h2>
		<p class="note">
			現在この環境は試運転モードです。この PC からは試運転の
			grant（ログイン不要の管理者相当のセッション）で管理操作ができる状態のため、現場での試運転が
			終わったら運用開始前に必ずロックダウンしてください。<strong
				>ロックダウンは元に戻せません</strong
			>（UI からは試運転モードへ戻せません）。
		</p>

		{#if hasAdminAccount === false}
			<p class="error">{NO_ADMIN_MESSAGE}</p>
		{:else if adminCheckError}
			<p class="error">管理者アカウントの確認に失敗しました: {adminCheckError}</p>
		{/if}

		{#if lockDownError}
			<p class="error">{lockDownError}</p>
		{/if}

		<button
			type="button"
			class="danger"
			onclick={handleLockDown}
			disabled={lockingDown || hasAdminAccount !== true}
			title={lockDownDisabledReason ?? undefined}
		>
			{lockingDown ? 'ロックダウン中…' : 'ロックダウンを実行'}
		</button>
	</section>
{/if}

<style>
	section.commissioning {
		border-color: var(--banto-warning, #8a5a00);
	}
</style>
