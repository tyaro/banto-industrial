/**
 * `getAuthSettings()`（ログイン不要モード＋デスクトップ自動ログインの現在値、
 * spec M11）を Account/Connectivity/Security の3カテゴリで共有する最小限の
 * ストア（#359 chronogazer 分）。
 *
 * 元 `+page.svelte`（単一ページ）では1つの `$effect`（`if (!tauri) return;`）
 * が一度だけ `getAuthSettings()` を読み、3つのセクション
 * （LANアクセス＝現 ConnectivitySection、認証＝現 SecuritySection、
 * 自動ログイン＝現 AccountSection）がその結果を直接参照していた。
 * カテゴリを実ルートに分割すると、そのうちどれか1カテゴリにしか
 * マウントされないコンポーネントの `$effect` に初回ロードを任せた場合、
 * 他のカテゴリへ直接遷移したときに `authSettings` が `null` のままになる
 * （banto-hub が PR #371 の Copilot レビューで踏んだ `hubStatusStore` と
 * 同種の回帰 - そちらの doc comment 参照）。上流テンプレート v1.6.0 も同じ
 * 問題に当たっており、`authSettingsStore`/`systemInfoStore` の取得を
 * `settings/+layout.svelte` の `$effect` に引き上げることで解決している
 * （上流 PR #198 の Copilot レビュー由来）。本ストアも同じ方針に揃え、
 * 初回ロードを `settings/+layout.svelte` へ集約する（`tauri` 限定という
 * 既存条件は元 `+page.svelte` の effect と同じまま維持）。
 *
 * 保存直後の即時反映（`saveAuthSettings`/`submitEnableAutologin`/
 * `submitDisableAutologin`）は各 section が自前で `load()` を呼ぶ - 元
 * `+page.svelte` の `applyAuthSettingsToDrafts`/`reloadAuthSettings` 直接
 * 呼び出しと同じタイミング。
 *
 * `error` は元 `+page.svelte` の `authError`（page-level effect が
 * `getAuthSettings()` に失敗したときだけセットしていた）と同じものを保持する
 * - 初回ロードが `+layout.svelte` に移っても「読み込み失敗を認証セクションに
 * 表示する」という元の見た目を変えないため、`value` とは別にエラーだけを
 * 共有する（`SecuritySection.svelte` が読む）。
 */
import { getAuthSettings, type AuthSettings } from '$lib/banto/authAdmin';

class AuthSettingsStore {
	value: AuthSettings | null = $state(null);
	error: string | null = $state(null);

	/** `getAuthSettings()` を1回叩き、`value` を更新して結果を返す（呼び出し元は自分のフォームの draft にも反映できる）。成功したら前回の `error` はクリアする。 */
	async load(): Promise<AuthSettings> {
		const next = await getAuthSettings();
		this.value = next;
		this.error = null;
		return next;
	}

	/** `applyAuthSettings()` のように変更系 API が最新値を直接返す場合、再フェッチせずそのまま反映する（元 `applyAuthSettingsToDrafts` と同じ役割）。 */
	apply(next: AuthSettings): void {
		this.value = next;
	}
}

export const authSettingsStore = new AuthSettingsStore();
