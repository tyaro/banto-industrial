/**
 * T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2・§3.3、2026-09-02
 * オーナー決定「`writable` の既定 ON。ただし収集グループ単位で変更可
 * （条件付き適用）」）: 新規タグフォームの `writable` チェックボックスの
 * 既定値を決める、依存ゼロの純関数モジュール。
 *
 * **既定 ON をグローバルに適用すると登録が失敗するタグがある**
 * （design §3.3）:
 *
 * - **computed タグ**は `writable` にできない
 *   （`crates/banto-tags/src/tag.rs`「computed タグは writable にできません
 *   （値は式が決まります）」）。**この判定は `tag_kind` を見るだけで済み、
 *   プロトコル知識を必要としない**ため本モジュールが直接持つ。
 * - **Modbus の読み取り専用領域**（`1xxxx`=discrete input /
 *   `3xxxx`=input register）は登録時に拒否される（2026-09-01 オーナー
 *   決定A）。
 *
 * **2026-09-02 オーナー判断（S1-b0 分離）: 上記2つ目（アドレス領域による
 * 判定）は本モジュールに実装しない。** この規則は現在
 * `banto-plc`（`AddressArea`、プロトコルの正）と `banto-tags`
 * （`crates/banto-tags/src/tag.rs::modbus_read_only_area`、登録時検証用の
 * 狭い複製）の2箇所に既に存在し、UI 側に3つ目の手書きミラーを作ると
 * プロトコルが増えるたびに3箇所を直す羽目になる。方針は「プロトコル層で
 * 定義し、UI はサーバーから受け取ったデータを表示するだけ」に統一し、
 * そのデータ供給は別スライス S1-b0 で行う。
 *
 * そのため {@link canDefaultWritable}/{@link writableDefaultBlockedReason}
 * は**アドレス文字列を一切受け取らない**。代わりに `writableArea`
 * （`boolean | undefined`）という**外部から渡されるフラグ**を受け取る -
 * これが S1-b0 が用意する「このアドレスは書き込み可能な領域か」という
 * サーバー由来の判定結果の差し込み口になる。**現時点ではこの情報の供給元
 * が無い**ため、呼び出し側（`tags/+page.svelte`）は常に `undefined` を
 * 渡す - つまり今は「PLC タグなら（アドレス領域を問わず）既定 ON」まで
 * しか行わない。`writableArea === false` を渡す経路が S1-b0 で配線されれば、
 * この2関数は変更なしにアドレス領域による絞り込みも行えるようになる
 * （呼び出し側だけが変わる）。
 *
 * 本モジュールは**登録時の検証を一切変えない** - サーバー側の8段ゲート
 * （API キーのスコープ完全一致・書き込み受付トグル・レート制限・監査を
 * 含む）はそのまま効く。ここで決めるのはあくまで新規作成フォームを開いた
 * 時点の「チェックボックスの初期状態」という UI 側の既定値だけ。
 */
import type { TagKind } from './tagRegistryAdmin';

/**
 * design §3.3「PLC タグかつ書き込み可能な領域のときのみ」既定 ON を
 * 適用してよいか。
 *
 * - `tagKind !== 'plc'`（computed/internal）は常に `false` - internal
 *   タグは `writable` を禁止されてはいないが（`tag.rs` の
 *   `INTERNAL_TAG_KIND` 分岐に `writable` チェックが無い）、design の
 *   適用条件が明示的に「PLC タグ」に限定しているため、internal タグの
 *   チェックボックスは（禁止されているわけではないが）これまでどおり
 *   既定 OFF のまま - 手動で opt-in する対象という位置づけを変えない。
 * - `writableArea`（モジュール冒頭コメント参照 - S1-b0 が供給する予定の
 *   アドレス領域判定、現時点では常に `undefined`）が明示的に `false`
 *   のときだけ、PLC タグでも既定を適用しない。`undefined`（未配線）・
 *   `true`（書き込み可能と判明）はどちらも「既定を適用してよい」として
 *   扱う - 情報が無い間はブロックしない、という保守的すぎない既定。
 */
export function canDefaultWritable(
	tagKind: TagKind,
	writableArea: boolean | undefined = undefined
): boolean {
	if (tagKind !== 'plc') return false;
	return writableArea !== false;
}

/**
 * `writable` の既定を適用しない場合に利用者へ見せる理由。適用できる
 * （{@link canDefaultWritable} が `true`）場合は `null`。
 *
 * - `computed`: 「値は式が決まる」ため常に不可（既存の書き込み安全設定
 *   セクション自体が非表示になるので、この文言は主にツールチップ／補助
 *   説明用）。
 * - `internal`: design の適用条件（PLC タグ限定）に単に該当しないだけで
 *   「禁止」ではないため、ブロック理由としては返さない（`null`）- 手動で
 *   ON にできることが既存の挙動どおり伝わるよう、否定的な理由表示は
 *   出さない。
 * - `plc` かつ `writableArea === false`: S1-b0 が配線された後にこの分岐が
 *   実際に到達するようになる（現時点では呼び出し側が常に `undefined` を
 *   渡すため到達しない）。文言はプロトコル非依存の汎用表現に留める -
 *   「どのアドレス領域だから」の具体名（`1xxxx` 等）は S1-b0 側のデータが
 *   持つ情報であり、本モジュールはそれを知らない。
 */
export function writableDefaultBlockedReason(
	tagKind: TagKind,
	writableArea: boolean | undefined = undefined
): string | null {
	if (tagKind === 'computed') {
		return 'computed タグは値が式で決まるため、書き込み可（writable）にできません。';
	}
	if (tagKind !== 'plc') return null;
	if (writableArea === false) {
		return 'このアドレスの領域は読み取り専用のため、書き込み可（writable）にできません。';
	}
	return null;
}
