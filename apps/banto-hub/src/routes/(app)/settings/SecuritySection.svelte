<script lang="ts">
	/**
	 * セキュリティカテゴリ（試運転モードのロックダウン、設計 §5.6・
	 * 2026-08-30 オーナー決定）（#359 段階1）。元 `+page.svelte` の該当
	 * セクションから markup・state・関数を無改変で移した。
	 *
	 * 表示条件は `sessionStore.commissioningMode` の1つ（ロックダウン済み
	 * なら元々このフラグが false なのでセクションごと非表示になる - 実装
	 * 指示「ロックダウン済みのときはこの操作を表示しないこと」）。
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
	import { goto } from '$app/navigation';
	import { getAuthProvider } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { lockDown } from '$lib/banto/commissioning';
	import { listUsers } from '$lib/banto/usersAdmin';
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
		if (!sessionStore.commissioningMode) return;
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
			await lockDown();
			toastStore.push('success', 'ロックダウンしました。ログイン画面へ移動します。');
			// ロックダウン後は以後の全リクエストで認証が必須になる - この画面に
			// 留まらせると後続の管理 API 呼び出しが軒並み 401 になって壊れて
			// 見えるため、即座にログイン画面へ誘導する（実装指示のとおり）。
			// ここまで来た時点で試運転モード中の合成セッションしか無い可能性が
			// 高いが、万一実トークンを保持していた場合に備えて `logout()` で
			// 確実に破棄してから遷移する（`Header.svelte` の logout と同じ手順）。
			await getAuthProvider().logout();
			goto('/login');
		} catch (err) {
			lockDownError = errorMessage(err);
		} finally {
			lockingDown = false;
		}
	}
</script>

{#if sessionStore.commissioningMode}
	<section class="commissioning">
		<h2>試運転モードのロックダウン</h2>
		<p class="note">
			現在この環境は試運転モードです。認証なしで管理操作ができる状態のため、現場での試運転が
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
