/**
 * **サーバーが試運転モード（未ロックダウン）か**（Svelte 5 runes）。
 *
 * banto v3.0.0（ADR-0017）で試運転は grant のセッション（`kind === 'commissioning'`）に
 * なったが、「サーバーが試運転中か」と「このタブが試運転の grant で操作しているか」は
 * 別の軸（2026-10-04 オーナー指示）:
 *
 * - **サーバーの状態**（このストア、`GET /api/commissioning/status` の `lockedDown`）は、
 *   ロックダウンの欄（設定の「セキュリティ」カテゴリ、`settings/+layout.ts` の可視判定と
 *   `guardCategory`、`SecuritySection.svelte`）と、試運転を示す表示（`status/+page.svelte`）
 *   を出し分ける。**アカウントでログインしたままでもロックダウンできる**（初回セットアップで
 *   作った admin はそのままアカウントのセッションになり、ロックダウンにはまさにその admin が
 *   要る）。
 * - **セッションの種別**（`sessionStore.commissioningGrant`）は「誰として操作しているか」
 *   だけに使う。
 *
 * `lockedDown` が `null`（未取得・取得失敗）のときは `serverCommissioning` は false:
 * 「分からない」はロックダウン済みと同じ扱いで、欄を出さない側に倒す（v1 の
 * `shouldBypassLoginForCommissioning` と同じ安全側）。読み直すのは設定グループの
 * `+layout.ts` の load（毎回）と状態ページの表示時、ロックダウンの直後（`SecuritySection`）。
 */
import { fetchCommissioningStatusOrNull } from './commissioning';

class CommissioningStateStore {
	/** `GET /api/commissioning/status` の `lockedDown`。`null` = 未取得・取得失敗。 */
	lockedDown: boolean | null = $state(null);

	/** サーバーが試運転モード（未ロックダウン）だと**確認できている**か。 */
	readonly serverCommissioning: boolean = $derived(this.lockedDown === false);

	/** サーバーに問い合わせて更新する。失敗は `null`（reject しない）。 */
	async refresh(signal?: AbortSignal): Promise<boolean | null> {
		const status = await fetchCommissioningStatusOrNull(signal);
		this.lockedDown = status === null ? null : status.lockedDown;
		return this.lockedDown;
	}

	/** 自分の操作で状態が確定したとき（ロックダウン成功）に、問い合わせずに反映する。 */
	markLockedDown(): void {
		this.lockedDown = true;
	}
}

export const commissioningState = new CommissioningStateStore();
