/**
 * T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2「さらに収集グループ
 * 単位で既定値を変更できるようにします（オーナー決定）。『このグループの
 * 新規タグは既定で書込可／不可』を持ち、タグ側で個別に上書きできる形」）:
 * 収集グループ単位の「新規タグの `writable` 既定値」を持続化する。
 *
 * **設計判断（2026-09-02、実装時）: DB 列ではなく `localStorage` に置く。**
 * 実装前に検討した DB 列案（`collection_groups.default_writable`）は、
 * `banto_tags`（I1）が relay-wright・banto-collect とも共有するクレート
 * であることが調査で判明し、`CollectionGroupInput`/`CollectionGroup` の
 * フィールド追加だけで両アプリ・複数クレートの40箇所超の構造体リテラル
 * （`apps/relay-wright/core/**`、`crates/banto-collect/**` 含む）を
 * 同時に直す必要が生じた（banto-hub 単体の変更のつもりが、無関係な
 * アプリのビルドを壊す）。banto-hub 内で完結する側テーブル案
 * （FK 無しで `collection_group_id` をキーに持つ、`hub_retained_values`
 * と同じ流儀）も検討したが、REST 応答形状・pending queue 再生ロジック
 * （`rest.rs`）の両方に手を入れる必要があり、`cargo test --workspace`
 * を実行できない制約下でその変更を検証しきれないと判断した。
 *
 * 一方この既定値は「その場でチェックボックスの初期状態を1つ決めるだけ」
 * の UI 便宜機能で、実際の登録可否は既存の8段ゲート（`writable` の
 * 検証含む）がどのみち独立に守る（design §3.3「8段ゲートは撤去しない」）
 * ため、**取り違えても実害がない**。それなら `getUiSettings()`
 * （`$lib/banto/setup.ts` - banto-hub では `createLocalUiSettings()` に
 * 固定された `UiSettingsProvider`、テーマ設定 `settings.svelte.ts` が
 * 使っているのと同じ抽象）に乗せず直接 `localStorage` を使う方を選んだ:
 * 新規作成フォームを開く `openCreateDrawer()` は同期関数で、開いた瞬間に
 * チェックボックスの初期値を確定させたい - `UiSettingsProvider.get` は
 * 非同期（Promise）なので、使うと「フォームが一瞬 false で開いてから
 * 非同期取得後に true へ切り替わる」ちらつきが起きる。`settings.svelte.ts`
 * の `loadThemeMode`/`loadThemePreset` も同じ理由で `UiSettingsProvider`
 * を介さず直接 `localStorage` から同期読み込みしている - 同じパターンを
 * 踏襲した。
 *
 * **既知の制約（トレードオフ、オーナー確認事項）**: ブラウザの
 * `localStorage` はオリジン単位でこの端末・このブラウザにしか残らない -
 * 別の利用者や別端末からは既定値が見えず、常に全体既定（ON）から始まる。
 * DB 列ならこの制約は無いが、上記の理由で今回は見送った。将来
 * 「グループ既定を全利用者・全端末で共有したい」という要望が来たら、
 * `collection_groups.default_writable` の DB 列化（banto_tags 側の
 * 呼び出し元一括更新を伴う）を検討する。
 */

const KEY_PREFIX = 'banto-hub.collectionGroup.';
const KEY_SUFFIX = '.defaultWritable';

function storageKey(groupId: number): string {
	return `${KEY_PREFIX}${groupId}${KEY_SUFFIX}`;
}

/**
 * グループ `groupId` へ新規タグを登録するときの `writable` チェックボックス
 * の既定値。未設定（このブラウザでまだ変更したことがない）・
 * `localStorage` が使えない／アクセス自体が例外を投げる環境（SSR、
 * プライベートブラウジング、あるいは単体テストの Node ランタイムが
 * `--localstorage-file` 無指定で `localStorage` を「触っただけで
 * `SecurityError`」にする構成 - `typeof localStorage` の評価自体が
 * 例外を投げるため `typeof … === 'undefined'` という一見安全な判定文すら
 * ガードにならない。`writableDefault.test.ts` の隣に置く本モジュールの
 * テストはこの経路を通る）・保存値が不正のいずれでも **`true`**
 * （design の全体方針「既定 ON」に合わせる）。明示的に `'false'` を
 * 保存した場合のみ `false` を返す。
 *
 * このため `settings.svelte.ts::loadThemeMode` の
 * `if (typeof localStorage === 'undefined') return …` という書き方は
 * ここでは使わない - `typeof` の評価そのものを `try` の中に入れる。
 */
export function getGroupDefaultWritable(groupId: number): boolean {
	try {
		if (typeof localStorage === 'undefined') return true;
		return localStorage.getItem(storageKey(groupId)) !== 'false';
	} catch {
		return true;
	}
}

/**
 * `getGroupDefaultWritable` が返す既定値を変更する。`CollectionGroupDrawer.
 * svelte` の「このグループの新規タグは既定で書込可」チェックボックスから
 * 呼ぶ - 保存に失敗しても（`localStorage` 不可の環境、上の doc comment
 * 参照）例外を投げず無視する（利便性機能であり、これで保存操作自体を
 * 止めない - 他の best-effort 書き込み、`settings.svelte.ts::
 * persistRemote` と同じ方針）。
 */
export function setGroupDefaultWritable(groupId: number, value: boolean): void {
	try {
		if (typeof localStorage === 'undefined') return;
		localStorage.setItem(storageKey(groupId), value ? 'true' : 'false');
	} catch {
		// 保存できなくても致命的ではない - 次回開いたときも全体既定（ON）
		// で始まるだけ。
	}
}
