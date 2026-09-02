/**
 * relay-wright の同名ファイルから複製。現在セッションの identity/role
 * （Svelte 5 runes）。
 *
 * 差分1: `authDisabled` は常に false 固定に単純化した。relay-wright の
 * ログイン不要モード（Tauri のみ・spec M11）は banto-hub のスコープ外
 * （実装指示「スコープ外: ...自動ログイン設定セクション」）なので、
 * `isTauri()`/`getAuthSettings()` への依存自体を削除している（banto-hub
 * は Tauri を持たない headless axum サーバーのみ）。
 *
 * 差分2（試運転モード、設計 §5.6・2026-08-30 オーナー決定）: `commissioningMode`
 * と `enterCommissioningMode()` を追加した。`(app)/+layout.ts` のルート
 * ガードが `$lib/banto/commissioning.ts` の `shouldBypassLoginForCommissioning`
 * でログインを迂回すると判断したときだけ呼ばれる。
 */
import { getAuthProvider, type Identity } from '@banto/admin-core';
import { parseRole, type Role } from './permissions';
import { COMMISSIONING_IDENTITY } from './banto/commissioning';

class SessionStore {
	identity: Identity | null = $state(null);
	role: Role = $state('viewer');

	/** banto-hub には Tauri のログイン不要モードが無いため常に false。 */
	authDisabled = $state(false);

	/**
	 * 試運転モード（未ロックダウン）中か。設定画面のロックダウンセクション
	 * の表示条件はこのフラグで決まる（`status/+page.svelte`「サーバー状態」
	 * の表示にも使う）。
	 *
	 * T19 S1-d（UX-45、2026-09-03）: 常時表示していた警告バナー
	 * （`(app)/+layout.svelte` の `CommissioningBanner.svelte`）は撤去した
	 * - このフラグ自体・ロックダウンセクションの表示条件は変えていない。
	 */
	commissioningMode = $state(false);

	/** 現在の identity を取得し、role を導出する（フェイルクローズ - parseRole 参照）。 */
	async load(): Promise<void> {
		this.commissioningMode = false;
		this.identity = await getAuthProvider().getIdentity();
		this.role = parseRole(this.identity);
	}

	/**
	 * 試運転モード用の初期化。**ネットワークを叩かない** - `getIdentity()`
	 * はローカルに bearer トークンが無いと `/api/auth/identity` すら呼ばず
	 * `null` を返す（`$lib/banto/commissioning.ts` の `COMMISSIONING_IDENTITY`
	 * の doc comment参照）ため、通常の `load()` を試運転モードで呼んでも
	 * 「セッション無し（role: viewer）」にしかならず、admin-only の
	 * ナビゲーション項目（設定画面のロックダウン操作を含む）が軒並み
	 * 見えなくなってしまう。サーバー側が試運転モード中は無条件に admin
	 * 相当としてリクエストを受け付ける（`actor_identity`）事実をフロント
	 * 側の RBAC 表示に反映するため、合成 identity をその場で設定する。
	 */
	enterCommissioningMode(): void {
		this.identity = COMMISSIONING_IDENTITY;
		this.role = parseRole(this.identity);
		this.commissioningMode = true;
	}
}

export const sessionStore = new SessionStore();
