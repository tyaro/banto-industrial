# 変更履歴 (Changelog)

banto-industrial のリリースノート。日付は JST。バージョンは [SemVer](https://semver.org/lang/ja/) 準拠（`publish = false` のワークスペースで、タグはリポジトリ状態の目印）。

## v0.2.0-alpha.24 — 2026-09-24（アルファ）

banto v1.7.0 に追従し、ユーザーの削除・降格・パスワード変更・パスワードリセットで、そのアカウントの既存のセッションが終わるようにした（banto #204）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。**wire 変更なし**（ただし、下記のとおり照合できないときの応答に `500` が増えた）。

### 修正（2026-09-24、banto v1.7.0 #204）

- **削除・降格・パスワードの変更やリセットをしても、そのアカウントでログイン済みのほかの端末のセッションが使えたままだった**。セッションはログイン時のロールを持ち続け、「ログインしたままにする」（Remember me）なら最長 30 日有効だった。`users` に認証の世代（`auth_epoch`）を足し、セッションをログイン時の「アカウント + 世代」に結び付けて、**要求のたびにアカウントと照合する**ようにした。ロールが変わる更新・パスワードの変更・リセットは、変更と同じ SQL 文で世代を進める。照合でアカウントが無い・世代が違えば、そのセッションは次の要求で `401` になり、ログイン画面へ戻る。一致すれば、DB の**今の**ロールで認可する。対象は管理 UI の API（`/api/*`）と、タグ空間 `/api/v1/*` をセッションで読む経路（API キーは従来どおり）。
- **自分のパスワード変更では、変更したセッションだけが続く**（ほかの端末のセッションは終わる）。表示名だけの変更では、セッションは終わらない。
- **DB が照合に答えられないときは、セッションを残したまま、その要求だけを失敗させる**（`500`）。画面は「ログイン状態を確認できませんでした」と再試行のボタンを出し、ログイン画面へは移らず、保存しているトークン（Remember me を含む）も消さない。
- **DB が応答しないときも、非常停止（`POST /api/write-control/disable`）だけは受け付ける**（#431）。照合をそのまま入れると、DB 障害のあいだ REST の非常停止まで `500` で通らなくなるため。それまで有効だった admin のセッションなら、照合できない（エラー・5 秒のタイムアウト）ときも停止を受け付け、監査の `detail` に `"sessionCheck": "unverified_stop_exception"` を残す。失効が確認できたセッションは拒否し、有効化（`enable`）など止める以外の操作は例外にしない。例外の対象はルート名ではなく、ハンドラの隣の宣言（`OperationKind::StopWrites`）で決める。MCP（API キー）の `set_write_control` は対象外（API キーには「それまで有効だった」ことを示すメモリ上の状態が無い）。
- **試運転モード（ロックダウン前）はトークンを発行しないので、今回の照合の対象外**。ロックダウンした時点から、ログインしたセッションはすべて照合される。
- 既存の DB は、起動時に `users.auth_epoch`（既存の行は 0）を足す。セッションはメモリにしか無いので、更新前のセッションの移行は無い（再起動でいずれにせよ切れる）。

### 変更（内部、2026-09-24）

- banto の依存（`banto-core` / `banto-storage` / `banto-server`、`@banto/*`）を `v1.7.0` に上げた。未保存の変更の確認（banto #214、`@banto/forms` の `guardUnsavedChanges`）の画面への適用は次の PR で行う。

## v0.2.0-alpha.23 — 2026-09-24（アルファ）

時系列データの保存層（banto-tstore）が、検索範囲によって読み出しで取りこぼす時刻の行を書き込み時に拒否するようにした（issue #424、オーナー決定 案 A）。banto-hub の収集（banto-collect）に効く。配布物の構成・前提ランタイムは alpha.3 以降と同じ。**wire 変更なし**。

### 修正（2026-09-24、#424）

- **行の記録時刻とデータファイルの日付を、契約として揃えた**。データファイルは名前の日付だけで選ばれる（読み出しの候補選び、保持期間の削除とも、ファイルを開かずに判断する）ため、ファイルの日付から離れた時刻の行は、**その時刻だけを含む狭い期間を指定するとファイルが候補から外れ、行を取りこぼしていた**（ファイルの日付も含む広い期間なら読めるので、期間の取り方で結果が変わっていた）。書き込み（`TsWriter::append`）は、読み出しの候補選びと同じ述語・同じ余白の定数で判定し、外れた行（ファイルの日付の UTC 0 時から −24h〜+48h の外）をエラー（`PtimeOutsideFileDate`）で拒否する。これで、**受け付けた行は、その時刻を含むどの期間を指定してもファイルの選択から漏れない**。拒否した行はバッファにも入れず、書かない。ただし、判定の前に行う通常の日付の切り替え（それまでの未書き込みの行の書き出しと、新しい日付のファイルへの切り替え）は起こりうる。日付が変わっていなければ、開いているファイルや他の未書き込みの行はそのまま。
- **通常の収集は挙動が変わらない**。記録時刻はファイルの日付を決めるのと同じ時計から取るので、日付の変わり目・夏時間・時計の巻き戻し（NTP・手動修正）では拒否されない（テストで確認）。拒否されるのは、記録時刻を取ってから書き込むまでの間に時計が大きく（24h − |UTC オフセット| 以上）飛んだ瞬間の 1 サンプルだけで、これは**捨てる**。収集は既存の「書き込み失敗」として扱い（ログ出力と、失敗の始まり・復旧のイベント）、次の書き込みから戻る。収集は止まらず、捨てたサンプルの再試行もしない。

## v0.2.0-alpha.22 — 2026-09-24（アルファ）

狭幅の退避ツリー（`SplitPane.svelte`）も、開く遷移に `visibility` を載せない形に揃えた（issue #421、#399/#420 と同型）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。**wire 変更なし**（フロントエンドのみ）。

### 修正（2026-09-24、#421）

- **`SplitPane.svelte`（タグ登録・タグモニタの狭幅退避ツリー）が、開く遷移でも `visibility` を 0.2s かけて遷移させていた**（#399 で直したサイドバーと同じ書き方）。Esc の層判定（`escLayering.ts`）はこのペインを層として扱わないため #399 と同じ誤動作は起きないが、開いた直後にペイン内の最初の要素へフォーカスを移す `SplitPane.svelte` 内の処理（`focusFirstInLeftPane()`）が、`visibility: hidden` のままの一瞬に `.focus()` を呼び空振りする余地があった。`Sidebar.svelte` と同じ形（`.open` 側だけ `visibility 0s`、閉じる側だけ遷移）に揃えた。見た目のスライドは変わらない。

## v0.2.0-alpha.21 — 2026-09-23（アルファ）

狭幅でサイドバーを開いた直後の Esc が、サイドバーより先に退避ツリーを閉じることがある不具合を直した（issue #399）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。**wire 変更なし**（フロントエンドのみ）。

### 修正（2026-09-23、#399）

- **狭幅（≤900px）でサイドバー（☰）を開いた直後に Esc を押すと、手前のサイドバーではなく下の退避ツリーが閉じることがあった**。サイドバーの `visibility` を開くときも 0.2s かけて遷移させていたため、開いてから最初の描画フレームが進むまで「非表示」扱いのままになり、Esc の層判定がサイドバーを見落としていた（退避ツリーの中にフォーカスがあるときに踏む）。開くときは即座に表示扱いにし、閉じるときだけ従来どおり遷移の最後に非表示へ切り替える。見た目のスライドは変わらない。CI の E2E でだけ時々落ちていたテスト（`banto-hub-viewport-tree-offcanvas.spec.ts` のテスト31）の原因がこれだった。

## v0.2.0-alpha.20 — 2026-09-16（アルファ）

狭幅（≤900px）でタグ登録・タグモニタの左ツリーをオフキャンバスへ退避するようにした（issue #378）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。**wire 変更なし**（フロントエンドのみ）。

### 変更（2026-09-16、#378）

- **狭幅ではタグ登録・タグモニタの左ツリーが画面外へ退避し、グリッドが全幅を使えるようになった**。400px 幅では固定 280px の左ツリーに押されてグリッドに約 120px しか残らず、行がクリックできなかった（#375 の E2E を書く過程で実測）。退避の型はサイドバーのオフキャンバス（≤900px）と同じで、境界も同じ `(max-width: 900px)` — 新しいブレークポイントは作っていない。
- **ツリーはツールバーの「📁 ツリー」ボタンで開き、グリッドの上に重なって出る**。ノード（接続・収集グループ・すべて）を選ぶと自動で閉じ、Esc とバックドロップのクリックでも閉じる。閉じたあとはフォーカスが開いたボタンへ戻る。閉じていても何で絞り込まれているかが分かるよう、ボタンの隣に現在の選択（接続名 / グループ名 /「すべて」）を出す。
- **広幅の見た目・操作は変わらない**（DOM・CSS とも従来どおりの2ペイン固定）。リサイズ可能なスプリッタは引き続き入れていない（2026-08-08 の見送りを維持）。タブ切替も採らなかった。

### 変更（内部、2026-09-16、#378）

- 退避は汎用部品 `SplitPane.svelte` 側に実装し、タグ登録とタグモニタで共有する。**同部品はアプリ非依存の規約を維持**し、狭幅かどうかは `narrow` prop で受け取る（`mobileNavStore` を import しない）。追加した props は `narrow` / `leftOpen`（`$bindable`）/ `leftLabel` / `leftId` / `focusFallback`（ペインが退避して不活性になるとき、中にフォーカスがあれば呼び出し側が返す要素へ逃がす）。
- Esc は「開いている退避ツリーが最優先」で、左ペインの `keydown` で `stopPropagation` して他の Esc ハンドラ（「一覧から挿入」トグル・式欄の補完）へ届かせない。閉じているときは何もしない。手前に出るモーダル（`Drawer`/`Modal`、z-index 900）が開いているときはそちらに譲る（退避パネルは 610、バックドロップ 600）。

## v0.2.0-alpha.19 — 2026-09-16（アルファ）

演算タグの式欄にセグメント補完を追加した（issue #342 段階B）。これで #342 の 3 段階（A: 式チェック API / B: セグメント補完 / C: 一覧から挿入）がすべて揃った。配布物の構成・前提ランタイムは alpha.3 以降と同じ。**wire 追加**: `GET /api/tags/expression/functions`（既存 API の互換は維持）。

### 追加（2026-09-16、#342 段階B）

- **演算タグの式欄でセグメント補完ができるようになった**。キャレット直前の字面から「いま何を補完すべきか」（接続名 / 収集グループ名 / タグ名）を判定し、候補をポップアップで出す。接続・グループを確定すると**名前 + `.`** が入ってそのまま次の階層が開き、タグを確定すると参照が完成して閉じる。確定は Enter / Tab / クリック、移動は ↑↓。挿入は「一覧から挿入」と同じ作法なので、確定のたびに段階Aのライブチェックが走る。
- **開くきっかけはドットの入力・2文字以上の入力・`Ctrl+Space`・`Ctrl+.`**。`Ctrl+Space` は Linux の ibus や Windows の IME が入力ソース切り替えに取ることがありアプリまで届かない環境があるため、`Ctrl+.` を併用できるようにしてある。**IME 変換中は開かない**（段階Aのチェックと同じ抑止）。
- **組み込み関数（`if`/`min`/`max`/`abs`/`round`/`clamp`/`bit`）も第1セグメントの候補に混ざる**（接続名のあと）。確定すると `name(` が入る。呼び出し形と1行説明も候補に添えて出す。
- **候補に出さないもの**: 式で表せない名前（日本語名・末尾ハイフンなど）と、段階C と同じ除外4種（自タグ・式で表せない名前・文字列型・循環）。段階C の行クリックが「淡色＋クリック時にトースト」なのに対し、**補完では最初から出さない**（打鍵の続きに出るものなので、選べない候補は矢印キーで踏むぶんだけ邪魔になるため）。
- **補完は狭幅の中央モーダルでも効く**（キーボードだけで完結し、グリッドを使わないため）。広幅の右ペイン限定である「一覧から挿入」トグルとはこの点が違う。
- **`Esc` はポップアップが開いている間はポップアップだけを閉じる**（「一覧から挿入」トグルは ON のまま）。ポップアップが閉じているときの `Esc` は従来どおりトグルを OFF にする。

### 追加（wire、2026-09-16、#342 段階B）

- **`GET /api/tags/expression/functions`**（editor 以上 - `POST /api/tags/expression/check` と同じ認可）を追加した。応答は `{ "functions": [{ "name", "arity", "signature", "description" }] }`。**組み込み関数の表は `banto-expr` の1箇所**（型検査が関数名と引数個数を引くのと同じ表）を正とし、それをこの API で UI へ配る - フロントエンドに関数表を書き写さない（2つに割れてドリフトするのを避けるため）。内容は静的なのでクライアントは1回取得してキャッシュする。**MCP には追加していない**（補完は UI 専用）。

### 変更（内部、2026-09-16、#342 段階B）

- `banto-expr` の組み込み関数表を `pub const BUILTIN_FUNCTIONS: &[BuiltinFunction]`（名前・引数個数・呼び出し形・説明）として公開した。型検査（`check_call`）は引き続きこの表から名前と引数個数を引く（表を2つにしない）。引数の型検査は従来どおり関数ごとの `match`。
- タグ名の「式に書けるか」判定をセグメント単位（`isExpressionRepresentableSegment`）へ切り出し、完全名の判定（`isExpressionRepresentableName`、段階C）もそれを使う形に書き直した（判定を2箇所に分けない）。
- contenteditable 化は採らなかった（issue #342 の「見送り」のとおり。`textarea` + ポップアップのまま）。参照の stable ID 化も別論点として扱わない。

## v0.2.0-alpha.18 — 2026-09-15（アルファ）

演算タグの式欄に「一覧から挿入」を追加した（issue #342 段階C。段階B「セグメント補完」は未着手）。あわせてタグの新規作成フォームも非モーダルの右ペインへ移した（#375 の続き）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。既存 API・wire は無変更（フロントエンドのみ）。

### 追加（2026-09-15、#342 段階C）

- **演算タグの式欄に「一覧から挿入」トグルを追加した**（`tagKind === 'computed'` かつフォームが右ペインに出ているときのみ表示）。ON の間だけ中央グリッドの行クリックが「そのタグの完全名を式欄のキャレット位置へ挿入」になり、**編集対象は切り替わらない**（破棄確認も出ない）。挿入後もフォーカスは式欄に残り、段階Aの式チェックがそのまま走る。
- **挿入できないタグはクリックしても挿入せず、理由をトーストで出す**: 自タグ・式で表せない名前のタグ（日本語名・空白入り・末尾ハイフンなど - レジストリの名前検証は式の識別子文法より広い）・文字列型タグ・（推移的に）自タグを参照している演算タグ（＝循環）。ON の間はこれらの行を淡色にする。**この判定は UI の補助で、正は段階Aのサーバチェック**（`error.kind` の `cycle`/`string_ref`）。
- トグルが自動で OFF になるのは、ペインを閉じた／編集対象が変わった／タグ種別が `computed` 以外になった／狭幅へ移った／表編集モード・複数選択モードへ入った／**Esc**（ペイン自体は閉じない）。**表編集モード・複数選択モードとは相互排他**で、そのあいだトグル自体が押せない（どちらも行クリックの意味が競合するため。これらのモードを自動で終了させることはしない）。
- issue 本文の原案（`ConnectionTree` を3階層にしてツリーのタグをクリックする）は採らなかった。#375 で編集フォームが非モーダルになり、既存の2階層ツリー＋グリッドのまま「グループで絞って行をクリック」で足りるため（2026-09-15 オーナー決定）。ツリーの構造は変更していない。

### 修正（2026-09-16、#342 段階C）

- **`-` に隣接するタグ参照の見落としを修正した**。式からタグ参照を抽出するクライアント側の近似正規表現（`tagDeleteImpact.ts`）が「直前が `-`」の候補を一律に除外していたため、`1-line1.fast.tag`・`-line1.fast.tag`・`(a.b.c)-line1.fast.tag` のように演算子の `-` に隣接する参照を見落としていた。影響は (1) 「一覧から挿入」の循環除外が効かず、循環になるタグを挿入できてしまう（保存時はサーバーが `cycle` で拒否する）、(2) **タグ削除前の参照影響表示（TAG-UX-C、alpha 以前からの既存バグ）でその形の参照元を見落とす**、の2点。`-` を識別子へ吸収するのは「直前が識別子のときだけ」という lexer の規則（`a-line1.fast.tag` の参照は `a-line1.fast.tag`）に合わせて判定を直し、期待値は `crates/banto-expr/tests/compile.rs` に実 lexer 対象のテストとして固定した。wire・サーバー側の挙動は無変更。

### 変更（2026-09-15、#375 の続き）

- **タグの新規作成フォームも広幅では非モーダルの右ペインへ移した**（編集・連続登録と同じ型・同じ幅 480px）。新しい演算タグの式を書く場面が一番多いのが新規作成で、「一覧から挿入」は非モーダルでしか成立しないため。
- **狭幅（≤900px）では従来どおり中央モーダル**（`Modal.svelte`）へフォールバックする。トグルはこのとき出さない（オーバーレイの下のグリッドを触れないため）。
- 「登録して次へ」「登録して閉じる」や複製元差分パネル・preflight 表示など、新規作成の挙動そのものは変更していない（入れ物が変わっただけ）。構造体展開・CSV 取り込みは引き続きモーダル。

## v0.2.0-alpha.17 — 2026-09-15（アルファ）

オーナー報告「編集中に左ツリーと中央グリッドを見られない」の解消（issue #375）。タグ画面の「編集」「連続登録」をモーダルの Drawer から非モーダルの右ペインへ移した。配布物の構成・前提ランタイムは alpha.3 以降と同じ。既存 API・wire は無変更（フロントエンドのみ）。

### 変更（2026-09-15、#375）

- **タグの編集と連続登録を非モーダルの右ペインへ移した**。オーバーレイを持たないので、編集中も左ツリーと中央グリッドをそのまま操作できる。幅は固定（編集 480px / 連続登録 640px — 従来の Drawer と同じ値）で、リサイズ可能スプリッタは入れていない。
- **Esc ではペインを閉じない**。閉じるのは明示的な「閉じる」ボタンと、連続登録の登録成功後だけ。開いた瞬間にフォーカスを奪うこともしない（`role="dialog"`/`aria-modal` は付けず、`<aside aria-label>` にした）。
- **狭幅（≤900px）は従来どおりオーバーレイの Drawer へフォールバックする**。ブレークポイントはサイドバーのオフキャンバス化と同じ `mobileNavStore.isNarrow` を流用し、新設していない。
- **構造体展開と CSV 取り込みはモーダルのまま**（一過性のウィザードのため）。接続・収集グループ・Sink グループの Drawer も今回は変更なし。
- 未保存のまま別タグを選んだときの確認（`selectTag` → `confirmDiscardIfNeeded`）は現行どおり。保存・検証・エラー表示・式チェックの各経路も無変更で、マークアップの入れ物が変わっただけ。

## v0.2.0-alpha.16 — 2026-09-15（アルファ）

オーナー報告「設定中に操作ミスで閉じてしまい最初からやり直しになる」の修正。`Drawer.svelte`/`Modal.svelte` が未保存の変更中は Esc・オーバーレイクリックでは閉じないようにした。配布物の構成・前提ランタイムは alpha.3 以降と同じ。既存 API・wire は無変更（フロントエンドのみ）。

### 修正（2026-09-15）

- **未保存のとき Esc・オーバーレイクリックで Drawer/Modal を閉じないようにした**。`Drawer.svelte`/`Modal.svelte` に `dirty` prop を追加し、`true` の間はこの2経路を無効化する - `×` ボタン経由の確認（`onRequestClose`）はそのまま塞がない。
- **接続 Drawer・収集グループ Drawer・Sink グループ Drawer には未保存確認そのものが無かった不具合を修正**。`ConnectionDrawer.svelte`/`CollectionGroupDrawer.svelte`/`SinkGroupDrawer.svelte` の `onRequestClose` はこれまで busy 判定のみで、未保存の入力があっても Esc・オーバーレイクリック・`×` のいずれでも確認なしに即閉じていた。タグ編集画面と同じ `isFormDirty` ベースの破棄確認（`window.confirm('変更を破棄しますか？')`）を追加した。新規作成ウィザード（3ステップ）でも同様に効く。
- タグ編集 Drawer は既存の未保存確認（`confirmDiscardIfNeeded`）に変更なし - 今回は Esc・オーバーレイクリックの誤爆防止を追加しただけ。
- ペイン構成（右ペイン常設化）の見直しは別 issue で扱う。

## v0.2.0-alpha.15 — 2026-09-15（アルファ）

演算タグの式チェック API・エラー位置のインライン表示・ライブプレビューを追加した（issue #342 段階A。段階B「セグメント補完」・段階C「ツリーからの挿入」は別 PR）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。既存 API・wire は無変更（今回は追加のみ）。

### 追加（2026-09-15、#342 段階A）

- **`POST /api/tags/expression/check`**（管理系ルーター、editor 以上）: 演算タグの式1本を保存せずに検証する。body は `{ expression, externalName? }`、応答は常に 200 で `{ ok, resultType, refs, preview, error }`（式が不正でも `ok: false` で返す - プレビュー用 API のため HTTP エラーにしない）。構文・型検査に加え、参照タグの存在確認・型/単位の解決・文字列タグ参照の拒否、`externalName` 指定時の循環参照判定、現在値による試算（参照が全件 Good のときだけ）を行う。
- **MCP ツール `check_expression`**（admin スコープ必須）: REST と同じ検証ロジックを共有する（`crate::rest::evaluate_expression_check`）。MCP ツール数は 37 → 38。
- **管理 UI（タグ登録画面）**: 演算タグの式欄が入力のたびに 300ms debounce で上記 API を叩き、構文・型エラーの位置（`pos`、バイト = 文字オフセット）に下線を表示し、結果型・参照タグ一覧（型・単位）・試算値をその場でプレビューする。IME 変換中は送信しない。エラーメッセージはクリック可能で、クリックしたときだけキャレットがエラー位置へ移動する（issue 原文の「入力のたびに自動でキャレットを飛ばす」は打鍵の邪魔になるため見送った）。

## v0.2.0-alpha.14 — 2026-09-15（アルファ）

banto-hub の管理 UI「設定」画面を、上流テンプレート v1.6.0（#358 で追従）と同じカテゴリ別ルートへ分割した（issue #359 の banto-hub 分。chronogazer・relay-wright への展開は別 PR）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。API・DB・wire は無変更。

### 変更（2026-09-15、#359）

- **管理 UI の設定画面をカテゴリ別ルートへ分割**。`/settings` は `/settings/{appearance,account,connectivity,data,security}` の実ルートに分かれ、`/settings` 自体と非可視カテゴリへの直接遷移は先頭の可視カテゴリへ 307 redirect する（**ブックマークは引き続き有効**）。カテゴリナビは 1024px 以上で左レール（sticky）、それ未満で横タブ。
- カテゴリ割り当て: テーマ/プリセット→`appearance`、アカウント（パスワード変更）→`account`、MQTT 発行/QoS・gRPC→`connectivity`、データ保持・構成の保存/読み込み（バックアップ）→`data`、試運転モードのロックダウン→`security`。
- コマンドパレット `config.import` の誘導先が `/settings#config-package` から **`/settings/data#config-package`** に変わった（`id="config-package"` の要素自体は無改変）。
- 各設定セクションの markup・文言・API は変更していない（section 単位のコンポーネント分割のみ）。ページ全体で見るとカテゴリナビとルートのラッパー（`.settings-layout` 等）が新たに加わっている（PR #371 の Copilot レビュー指摘を受けて記述を訂正）。

## v0.2.0-alpha.13 — 2026-09-15（アルファ）

PLC が到達不能の間、`collect_events` に `plc_reconnected` と `plc_disconnected` が収集周期ごとにフラップし続ける不具合の修正。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 修正

- **PLC 到達不能中に `plc_reconnected`/`plc_disconnected` が毎秒フラップする不具合を修正**（#344）。抜線などで PLC が落ちている間、接続ごとに毎秒 1 組（1000ms 周期・2 接続で約 13,000 件/時）がイベントに積まれ続け、実際の再接続と区別できず、長時間の機器断で DB 肥大とイベント API の応答劣化を招いていた。原因は hub の `BrokerReadClient::connect()` が broker セッションの実状態を見ずに常に即 `Ok` を返していたこと - 収集ティックの読み取りが broker の `Disconnected` で失敗 → `plc_disconnected` → 直後の再接続が無条件に成功 → `plc_reconnected` → 次のティックでまた失敗、の繰り返しだった（broker 自身の再接続バックオフ 12〜15 秒とは無関係）。
  - `BrokerReadClient::connect()` は broker の `status_watch` を見るようになった。`Connected` なら即成功、`Reconnecting` なら 3 秒まで復帰を待ち、間に合わなければ失敗を返す（`Stopped`・broker タスク終了は即失敗）。これにより収集側のバックオフ（1s → 30s）が本来の役目を果たし、断のあいだ状態が `Connected` へ戻らなくなる。
  - **`plc_reconnected` の意味を明確化**: 「接続に成功した瞬間」ではなく **「切断後、最初の読み取りが成功した時点」** に 1 回だけ記録する。`ts` はその読み取りティックの時刻。`plc_disconnected` も同様に、既に記録済みの断が続いているあいだは再記録しない。実機・シミュレータどちらの経路でも同じ規則（`banto-collect` の接続タスク側で実装）。
- **wire 変更なし**。`collect_events.kind` の値・REST/gRPC/WS のスキーマはいずれも変更していない。変わったのは「いつ何件記録されるか」だけで、`plc_reconnected` を監視している下流（Thermal Monitor、DB Sink 経由の集計）は、これまで毎秒届いていた偽の再接続が届かなくなる。

### 既知の制限（アルファ）

alpha.11 と同じ。

## v0.2.0-alpha.12 — 2026-09-15（アルファ）

alpha.10（#335）で外部への読み取り出力のシミュレーションゲートを撤廃したため、どの経路からも参照されなくなっていた T15-3「テスト出力」（`test_output`）機構を撤去した。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 変更（2026-09-15、#362）

- **`TestOutputControl`・REST の `POST /api/test-output/{enable,disable}`・`GET /api/v1/status`/`GET /api/status` の `test_output`/`testOutput`・gRPC `StreamValuesRequest.test_output`・`ValueBatch.simulation`/`run_id` を機構ごと撤去した**。`apps/banto-hub/core/src/test_output.rs` を削除。`CollectionController`/`GrpcService`/`MqttPublisher` の test_output 連動配線もすべて削除。
- **wire 変更（破壊的）**: `GET /api/v1/status`・`GET /api/status` の応答から `test_output`/`testOutput` フィールドが消える。`POST /api/test-output/enable`・`POST /api/test-output/disable` は 404 になる。gRPC proto の `StreamValuesRequest.test_output`（旧 field 4）・`ValueBatch.simulation`（旧 field 3）・`ValueBatch.run_id`（旧 field 4）は `reserved` 化し、これらのフィールドを送っても無視される（送信自体は失敗しない）。
- MQTT publish・WebSocket（`/api/v1/stream`）は #364（alpha.10）までに「run mode によらず常時配信」へ移行済みで、本リリースでの追加変更は無い。

### 既知の制限（アルファ）

alpha.11 と同じ。

## v0.2.0-alpha.11 — 2026-09-15（アルファ）

シミュレーションデバイスのタグへの外部書き込みを、拒否せず PC 上のシミュレータへ反映するようにした。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 変更（2026-09-15 オーナー決定、#363）

- **シミュレーションデバイス（接続単位 `simulation: true`、および全シミュレーション運転中の接続）のタグへの外部書き込み（REST `POST /api/v1/values/{tag}` とバッチ、gRPC `WriteValue`、MCP）を拒否せず、PC 上の in-process シミュレータへ反映する**。実機が無い状態で SCADA 等のクライアントが書き込み経路まで試験できるようにするため。#335 で「外部出力＝読み取り出力であって PLC 書き込みではない」と整理したことに伴う追加決定。
- **実機向けの護りはすべてそのまま**適用する（per-tag `writable`、API キーの `write` スコープ、実効 enabled、プロトコル、`write_enabled`（受付トグル）、レート制限とトリップ、値変換・レンジ検査、log-before-write 監査、収集停止中の fail-closed）。撤去したのは「シミュレーション中は拒否」の 1 段（旧ゲート 4）だけ。
- **`banto-plc` のシミュレータが書き込みコマンドに対応**: Modbus は FC5（single coil）/ FC6（single register）/ FC15（multiple coils）/ FC16（multiple registers）、SLMP はデバイス一括書き込み `0x1401`（ワード単位・ビット単位の両サブコマンド）。
- **書いた番地は「保持（held）」される**: ワイヤ経由で書き込まれた番地は `banto-collect` のランプ波生成（100ms 周期、先頭 16 番地）の更新対象から外れるため、書いた値が次のポーリング・`GET /api/v1/values/{tag}/read-now` でそのまま読み戻せる。書いていない隣の番地はランプで動き続ける。保持はシミュレータインスタンスの寿命と同じで、接続の `simulation` 切替や全シミュレーション運転の開始・停止で消える。
- **wire 変更**: エラーコード `simulation_write_rejected`（REST 503 / gRPC `UNAVAILABLE`）は**返らなくなった**。このコードで分岐していた外部クライアントは、その分岐が不要になる。
- **監査**: log-before-write の `detail` に書き込み先の種別を記録するようにした（成功時 `{"target":"simulator"}` / `{"target":"plc"}`、失敗時は `{"target":"...","detail":"失敗理由"}`）。DB スキーマの変更（カラム追加）は無し。

### 既知の制限（アルファ）

alpha.9 と同じ。

## v0.2.0-alpha.10 — 2026-09-15（アルファ）

API キー経由の catalog/REST から computed タグとシミュレーション値が消えていた不具合の修正 + 外部への読み取り出力のシミュレーションゲート撤廃。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 修正

- **API キー経由の `GET /api/v1/tags`（catalog）から computed タグが常に消えていた不具合を修正**（#335）。原因は `value_source_for_tag` が computed タグに実行状態を問わず常に `derived_simulation` を返し、「シミュレーション由来の値は外部出力しない」というフィルタ（`api_key_external_output_allowed`）へ恒久的に引っかかっていたこと。同じフィルタの先頭 `if entry.simulation { return false; }` により、接続単位で simulation 設定済みの PLC タグも収集停止中は catalog から消えていた。WS/gRPC の購読経路にはこのフィルタが無く、値は届くのに catalog からは発見できない、という不整合があった（#332 の検討中に発覚）。

### 変更（2026-09-14 オーナー決定）

- `value_source` に新ラベル `computed` を追加。computed タグの既定は `computed`（wire 追加 - 既存クライアントは未知値として扱えること、`banto-tagclient` の `ValueSource::Unknown` は維持）。`derived_simulation` は「全 PLC シミュレーション運転中、または式が参照する入力タグのいずれかが実際にシミュレーション中（PLC タグの `effective_simulation == true`）」のときだけの**情報ラベル**に変更 - もう抑止フィルタの対象ではない。
- API キー経由でも catalog（`GET /api/v1/tags`）は収集状態を問わず常に全タグを返すようにした。`api_key_external_output_allowed` を撤去。
- `banto-tagclient` の `ValueSource` に `Computed`（`"computed"`）・`Db`（`"db"`、外部 DB 連携 2026-09-06 決定由来の既存ギャップ）を追加。

### 変更（2026-09-15 オーナー決定「外部出力を PLC への出力と勘違いしていた」で追補）

- **外部への読み取り出力（REST の `/api/v1/values` 系・WS・gRPC・MQTT）はシミュレーションで一切ゲートしないよう統一**。全 PLC シミュレーション運転中でも catalog・値読み取り・購読・publish を無条件で配信し、呼び出し側は `value_source`/`collection_mode` で判別する。
  - REST: `/api/v1/values`（一括）・`/api/v1/values/{tag}`・read-now の run 単位 503 `simulation_output_disabled` ゲートを撤去。
  - WS（`stream.rs`）: `handle_subscribe` の `RunMode::AllSimulation` 拒否、および tick/`runtime_rx.changed()` での「AllSimulation 突入時に外部購読を能動切断する」分岐を撤去。
  - gRPC（`grpc.rs`）: `simulation_output_disabled`/`simulation_output_disabled_status` ゲートと、テスト出力ストリームの自動終了ロジックを撤去。`StreamValuesRequest.test_output` は wire 互換のため受け付けるが無視する（**wire 変更なし、意味論のみ変更** - proto コメントに deprecated と明記）。
  - MQTT（`mqtt.rs`）: `eval_target` の「AllSimulation 中は test_output が有効なときだけ publish」ゲートを撤去し、run mode によらず通常トピックへ publish する。テスト出力専用トピック（`{prefix}/test/{run_id}/...`）・`TestOutputPayload`・`PublishTarget::Test` は撤去した。
  - 書き込み経路（`POST /api/v1/values/{tag}`・gRPC `WriteValue`・MCP）の simulation 判定は変更していない - 今回の決定は読み取り出力のみが対象。
- `test_output`（T15-3）の制御プレーン自体（`TestOutputControl`、`POST /api/test-output/{enable,disable}`、`GET /api/v1/status` の `test_output` 表示、UI トグル）は今回は残すが、どの出力経路も参照しなくなったため **deprecated**（効果なし）。撤去は後続 issue で行う。
- **MQTT ペイロードに `value_source` を追加**（JSON への追加フィールド、既存購読者は無視可）。`value_source_for_tag`/`effective_simulation_for_tag` を `crate::value_source`（`apps/banto-hub/core/src/value_source.rs`）へ共有モジュール化し、REST と MQTT の両方から同じ判定を使う。

### 既知の制限（アルファ）

alpha.9 と同じ。

## v0.2.0-alpha.9 — 2026-09-14（アルファ）

構成変更（CRUD）が実行構成へ届くまでの契約の変更。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 変更

- **試運転中は接続・グループ・タグの CRUD を収集中でも即時・無停止で反映し、ロックダウン後のみ pending queue + 明示適用にする**（#341、オーナー決定 2026-09-09 / 2026-09-14）。試運転は配線の誤りを直しながらタグを足す工程で、変更のたびに収集停止・適用・再開を挟む契約は工程と噛み合わないため。ロックダウン後は「構成凍結」の意図どおり queue + 明示適用のワンクッションを必ず挟む。

  | 状態                     | CRUD の受付                  | 実行構成への反映                                                 |
  | ------------------------ | ---------------------------- | ---------------------------------------------------------------- |
  | 試運転（未ロックダウン） | 収集中でもそのまま受け付ける | preflight 合格後に即時・無停止反映（pending queue を経由しない） |
  | ロックダウン済み         | pending queue へ保存（202）  | 明示適用したときのみ反映。**適用も無停止**                       |
  - 受付側の判定を `rest::registry_change_should_queue`（REST 12 箇所・MCP 9 箇所が共有）に集約。条件は「収集中 かつ ロックダウン済み」のときだけ queue。収集中を一律 409 `collection_edit_locked` にしていた `require_collection_stopped` は撤去した。
  - 反映側は `CollectionController::commit_catalog_and_apply_live` に一本化。catalog/演算 plan/DB Source plan/`configured_revision` をコミットし、収集が `Running` のときだけ `CollectorManager::apply_run(現在の run mode)` で実行構成まで無停止更新する（T7 の `apply_config` による接続単位の部分再構成。run mode のオーバーライドを尊重するので `AllSimulation` 運転中に実機へダイヤルし直すことはない）。`start`/`stop` と同じ `transition` ロックで直列化する。
  - `POST /api/pending-changes/{id}/apply` の収集停止要求（409 `collection_edit_locked`）を撤廃。適用しても収集は `Running` のまま、run mode も変わらない。
  - DB へはコミットできたが実行構成へ反映できなかった場合、200 + 警告ではなく `500 live_reconfigure_failed` を返す（走行中の収集は元の構成のまま無傷。理由は `GET /api/status` の `lastConfigError` にも残る）。
  - 管理 UI: タグ画面の表編集モードを「停止中、または試運転中」に緩和。状態ページに構成変更がいつ反映されるかの注記を追加。
  - 互換ルーター専用だった `legacy_live_reconfigure`（pre-T14-3 の live apply opt-in）は、この新経路に置き換わったため撤去した。
  - **`POST /api/collection/reapply`（admin）を追加**。反映だけが失敗した後の回復用に、最新の DB 内容を catalog と実行構成へ適用し直す（収集 `Running` なら無停止、`Stopped` なら catalog のみ）。収集のライフサイクル（start/stop/mode）には触らない冪等な操作で、`live_reconfigure_failed` のエラー文言からも案内する。MCP には追加していない。
  - catalog へ反映する registry snapshot は、呼び出し元のトランザクション内のものではなく `transition` ロック取得後に読み直した最新のものを使う（並行 CRUD で catalog だけが古くなる窓を閉じる）。呼び出し元の in-tx snapshot は保存前検証（preflight）専用。保存前検証には DB Source の計画検証も追加し、「保存成功 ＝ 実行可能」の保証を catalog・演算・DB Source の3つに揃えた。
  - `CollectorManager::apply_run` は collector 側の適用に失敗したとき、broker セッション集合を直前に適用成功した構成へ戻す（ベストエフォート。収集開始の失敗時にも効く）。catalog / 演算 / DB Source の検証失敗も `last_config_error` に残すようにした。
  - 未適用キューの適用が「DB は成功・実行構成への反映だけ失敗」で終わった場合、`result: "failed"` の監査行を残す（行自体は二重適用を防ぐため `applied` のまま）。

### 修正

- **`banto_collect::Collector::apply_config` の旧 writer 退避が、新タスクの spawn より前で `?` 伝播していた**（#341 レビュー）。旧ファイルの最終 flush に失敗すると「writer は新・`self.config` は旧・追加/置換した接続のタスクが1本も立っていない」状態で `Err` が返り、呼び出し元は失敗と判断する一方で収集は何も行わなくなる。退避を spawn の後へ移し、失敗はログのみで `Ok` を返すようにした（失われうるのは旧ファイルの最後の未フラッシュ分だけ）。
- **`banto_collect::Collector::apply_config` で、唯一の接続が置き換えられたときに新しい writer が収集タスクへ届かない不具合**（#341 で発覚）。`watch::Sender::send` は受信者が 0 人だと値を更新せずに `Err` を返すが、置き換え対象タスクを join した直後のこの地点では受信者が 0 になりうる（`Collector` 自身は受信者を保持しない）。結果、既存グループにタグを1本足すと、新タスクは毎周期 2 値を append するのに writer のスキーマは 1 列のままで、その接続の**履歴書き込みが列数不一致で全滅**していた（現在値と live event は流れ続けるため「値は見えるのに履歴が残らない」症状）。`send_replace` に変更して常に配布する。#341 以前は本番から到達しない経路だったため表面化していなかった。

### 既知の制限（アルファ）

alpha.8 と同じ。

## v0.2.0-alpha.8 — 2026-09-14（アルファ）

書き込み受付（`write_enabled`）トグルの挙動変更のみ。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 変更

- **書き込み受付の既定を「可」に変更し、再起動・収集操作での自動無効化を撤回**（#340、オーナー決定 2026-09-09）。banto-hub はルールエンジンを持たないパススルーで自律再開の危険が無く、書き込み可否の実体は per-tag `writable`（既定 false）と API キーの `write` スコープが担うため、グローバルトグルは運用者が手で止める非常停止スイッチに徹することにした。
  - `write_control_state.enabled_persisted` の seed を `1`（可）に変更。プロセス起動時はこの永続値をそのままライブフラグへ復元する（従来は永続値を無視して常に無効から始まっていた）。
  - 既存 DB では、運用者が一度も enable/disable を操作していない未操作行（`last_changed_at IS NULL`）だけを新既定の「可」へ引き上げる。明示的に無効化した設定（`last_changed_at` あり）はそのまま保持される。
  - 収集の開始・停止・モード変更（`CollectionController::start`/`stop`/`set_mode`）はもはや書き込み受付を無効化しない（test_output の OFF 連動は変更なし）。
  - `GET /api/v1/status` の `write_was_enabled_before_restart` は互換のためフィールド名を維持し、意味を「起動時に永続テーブルから復元した値」に変更した。
  - **永続化失敗時の挙動も同時に見直した**: `enabled_persisted` が次回起動時のライブ値そのものになったため、`POST /api/write-control/enable|disable`（MCP `set_write_control` も同様）は永続化の失敗を握りつぶさず、REST は 500 `write_control_persist_failed`、MCP は `isError: true` を返すようにした。disable は先にライブフラグを落としてから永続化を試みる（fail-closed。失敗してもトグル自体は無効のまま）。enable は永続化に成功したときだけライブフラグを立てる（失敗時は無効のまま変えない）。

### 既知の制限（アルファ）

alpha.7 と同じ（実 DB 検証 S7 未実施、`admin` スコープ API キーはサーバー全権、サイドカーはループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提、OPC UA #201 / SQL Server / SLMP イベント PUSH #258 は未実装、`i64`/`u64` は 2^53 超で精度低下）。

## v0.2.0-alpha.7 — 2026-09-08（アルファ）

`v0.2.0-alpha.6` の不具合修正のみ。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 修正

- **1 対 1 接続しか受け付けない Modbus/TCP 機器で収集が一切成立しなかった**（#337）。オムロンの電力量モニタ＆ロガー KM-D1-ETN（マニュアル KANC-718B 12-1）のように**同時接続を 1 本しか許さない**機器で、全タグが Bad のまま値が一度も更新されない状態だった。
  - 原因は **Modbus 接続がソケットを 2 本張っていた**こと。#131 で Modbus の書き込み／ステータスが banto-broker のセッション経由になった一方、**収集読み取りだけは直結の `ModbusTcpClient` のまま**だったため、収集中は「broker セッション 1 本 + 収集用の直結 1 本」になっていた。実機を TCP プロキシで観測すると、先に張られた broker セッションが無通信のまま居座り、収集用の 2 本目が即切断され、以後 1 秒ごとに再接続 → 即切断を繰り返していた。
  - 「多くの Modbus TCP サーバは同時接続を許す」という #131 当時の前提は、許す機器に対しては正しかったが、**許さない機器では機能不全**になる。2026-09-08 オーナー決定として [docs/tag-server-design.md](docs/tag-server-design.md) §6-5 のトレードオフを覆し、**SLMP と同様に収集読み取りも broker セッションへ相乗り**させた。banto-broker は元々「read / write が 1 セッションを共有する」ことが存在理由であり、Modbus だけ直結だったのが例外だった。
  - **管理 UI の「接続テスト」ボタンも同様に修正**した。こちらは毎回新規に直接ダイヤルしていたため、1 対 1 機器では**収集が正常でも「失敗」と誤診断**していた。SLMP 側（実機 R08ENCPU の同じ性質への対策として実装済み）と同じく、既存の broker セッションがあれば再利用する。
  - **トレードオフ**: 収集読み取りと書き込みが 1 本のソケット上で直列化される。SLMP が以前からそうであるのと同じ性質で、長い書き込みが収集ティックを遅らせうる。
  - 併せて 2 件の潜在バグを修正した。broker セッションの起動に失敗した接続が「読み取りが broker 経由」として扱われ、シミュレータだけ抑止されて実機ホストへダイヤルしていた問題と、Modbus 接続のシミュレーション切り替えが収集タスクの再生成を引き起こさず古いセッションを指したままになる問題（T9-2 と同型、今回の変更で新たに到達可能になるもの）。

### 既知の制限（アルファ）

alpha.6 と同じ（実 DB 検証 S7 未実施、`admin` スコープ API キーはサーバー全権、サイドカーはループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提、OPC UA #201 / SQL Server / SLMP イベント PUSH #258 は未実装、`i64`/`u64` は 2^53 超で精度低下）。**KM-D1-ETN 実機での 64bit 読み取り確認は引き続き未実施。**

## v0.2.0-alpha.6 — 2026-09-08（アルファ）

`v0.2.0-alpha.5` の不具合修正のみ。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 修正

- **収集ポーリングが接続のワード順設定を無視し続けていた**（#334）。alpha.5（#325 / #333）で `banto-collect` の config ビルダーに `word_order` を通したが、**ポーリングタスクはその値を使っていなかった**。`task.rs` の `ClientSpec` が `ProtocolConfig` の手作業の部分コピーで `word_order` フィールドを持たず、`client_spec()` が値を捨て、`default_client_factory()` が `..ModbusTcpConfig::default()` / `..SlmpConfig::default()` でクライアントを組み直す際に既定値（Modbus = `HighLow` / SLMP = `LowHigh`）へ静かに戻っていた。
  - **alpha.5 では `wordOrder: "low_high"` の Modbus 接続の f32 / f64 収集値が壊れたままだった**（f32 の `85.0` を下位ワード先行で置くと `2.39e-41` になる）。read-on-demand は broker 経路で `ProtocolConfig` を直接読むため正しくデコードしており、**同一タグの値が経路によって食い違っていた**。オムロン KM-D1-ETN や KEYENCE TR-W550 のように `low_high` で公開する機器が該当する。
  - `ClientSpec` に `word_order` を追加し、`client_spec()` と `default_client_factory()` の Modbus / SLMP 両アームで明示的に受け渡す。Modbus 側は `..ModbusTcpConfig::default()` を削除して全フィールドを列挙したので、**今後フィールドが増えたらコンパイルエラーで気づける**。
  - 回帰テストとして、実シミュレータのソケットに対して同一のワイヤビット列を `low_high` 接続と `high_low` 接続の両方で収集し、値が期待どおり分かれることをエンドツーエンドで固定した（f64 を含む）。
  - **既存環境への影響は無い**: migration 0017 適用後の Modbus 接続は `high_low` で修正前のハードコード値と一致し、SLMP はレジストリ既定 `low_high` が `SlmpConfig::default()` と一致する。挙動が変わるのは**明示的に既定と異なるワード順を設定した接続**、つまりこれまで壊れていた接続だけ。banto-hub は SLMP に broker アダプタを使うため、このバグを踏むのは **Modbus のポーリングのみ**。

### 既知の制限（アルファ）

alpha.5 と同じ（実 DB 検証 S7 未実施、`admin` スコープ API キーはサーバー全権、サイドカーはループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提、OPC UA #201 / SQL Server / SLMP イベント PUSH #258 は未実装、`i64`/`u64` は 2^53 超で精度低下）。**KM-D1-ETN 実機での 64bit 読み取り確認は引き続き未実施。**

## v0.2.0-alpha.5 — 2026-09-08（アルファ）

機能追加 1 件（Modbus の 64bit データ型）と、その作業中に発見した既存バグの修正 1 件（Modbus のワード順）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 追加

- **Modbus のタグに 64bit データ型 `i64` / `u64` / `f64` を追加**（#325）。1 タグ = **4 レジスタ（64bit）**を占有する。オムロンの電力量モニタ＆ロガー KM-D1-ETN のように、全計測項目を 4 ワードの IEEE754 倍精度で公開する機種を **1 タグとして読める**ようになった（従来は `u16` × 4 本のタグに分けて、クライアント側で `f64::from_bits` で組み立てる回避策が必要だった）。
  - **Modbus 接続配下のタグ限定**。SLMP / 予約接続（calc・mem）/ DB 接続（postgres）配下では `dataType` のバリデーションエラーになる。強制は `collection_groups` → `plc_connections` を JOIN する配置チェックで行っており、SQL の `CHECK` 制約には入れていない（`tag_kind` の calc/mem/postgres 規則と同じ方式）。管理 UI のデータ型プルダウンも、収集グループの接続が Modbus のときだけ 64bit 型を出す。
  - ワード順は**接続の設定**に従う（`high_low` = 先頭レジスタが最上位ワード / `low_high` = 4 ワードの並びを反転）。KM-D1-ETN は `low_high`（メーカーマニュアル KANC-718B §12.5）。
  - **書き込みにも対応**。`i64`/`u64` は整数・範囲チェック、`f64` は有限性のみ（非整数値を受け付ける）。
  - 連番登録・構造体登録・オフセットコピーのアドレス自動増分、タグ CSV の入出力も 64bit 型（+4）に対応。
  - **既知の制限**: 値は従来どおり `f64` で運ばれるため、`i64` / `u64` は **2^53（9,007,199,254,740,992）を超える整数で精度が落ちる**。積算カウンタ用途ではこの上限に注意すること。
  - relay-wright の書き込みターゲット（`write_targets`）は 64bit 型を受け付けない（SLMP 前提の relay-wright 固有リソースのため）。

### 修正

- **Modbus 接続のワード順設定が収集経路で無視されていた**（#325 の作業中に発見した既存バグ）。`plc_connections.word_order`（migration 0010）について、次の 3 つが噛み合っていなかった。
  - 収集ポーリング（`banto-collect`）は列を読まず **HighLow 固定**で動いていた。
  - read-on-demand と書き込み（`banto-broker`）は列を読んでおり、列の既定 `'low_high'` がそのまま効いて **LowHigh** で動いていた。
  - 管理 UI の接続フォームはワード順を **SLMP 選択時しか表示していなかった**ので、Modbus 接続には事実上必ず `'low_high'` が保存されていた。

  結果、既定設定の Modbus 接続では**同じ `u32`/`f32` タグの値が、収集経路と即時読み取り／書き込み経路でワード反転して食い違って**いた。オーナー決定として **Modbus は `'high_low'`（Modbus/IEEE 慣習）に統一**した。収集経路が列を読むよう修正し、broker 側の解釈をそれに揃えている。

  - **更新時の注意**: `banto_tags::migrate`（Hub 起動時に自動実行）で、**既存の `modbus-tcp` 接続の `word_order` が一律 `'high_low'` に書き換わる**（migration 0017）。これは収集経路の現行動作と蓄積済み履歴データの解釈を変えないための「実態への同期」だが、**MCP や REST を直接叩いて Modbus 接続に明示的に `'low_high'` を設定していた場合、その設定は失われる**。更新後に設定し直すこと。
  - **管理 UI の接続フォームで Modbus でもワード順を選べる**ようにした。新規 Modbus 接続の既定は `'high_low'`、SLMP は従来どおり `'low_high'`。
  - 構成パッケージのインポートでも、`wordOrder` を持たない旧パッケージの Modbus 接続は `'high_low'` として読み込む（当時の実挙動に合わせるため）。

### 既知の制限（アルファ）

alpha.4 と同じ（実 DB 検証 S7 未実施、`admin` スコープ API キーはサーバー全権、サイドカーはループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提、OPC UA #201 / SQL Server / SLMP イベント PUSH #258 は未実装）。加えて上記の `i64`/`u64` の 2^53 精度制限、および **64bit 型の実機検証（KM-D1-ETN 実機での読み取り）は未実施**。

## v0.2.0-alpha.4 — 2026-09-08（アルファ）

`v0.2.0-alpha.3` の不具合修正のみ。配布物の構成・前提ランタイムは alpha.3 と同じ。

### 修正

- **サービス起動失敗時に `Stopped` を二重報告していた**（#330）。起動失敗パスの `return` が `runtime.block_on(async move { .. })` に渡した async ブロックからしか抜けず、`run_service_body` 末尾の `report_status(Stopped, Win32(0))` が必ず実行されていた。実機ではログに `サービス状態の報告に失敗しました: IO error in winapi call` が残っていた（alpha.3 の実機検証で発見、設計 §8.3）。
  - 2 回目の報告は**正常終了（`Win32(0)`）**なので、Win32 API 呼び出しが失敗してくれるおかげで #327 のサービス固有終了コード（プロファイルロック保持 = `2` / その他の `HubStartError` = `3` / tokio ランタイム構築失敗・`Configured` 収集の開始失敗 = `1`）が SCM に残っていたにすぎない。API の失敗頼みをやめ、`Stopped` を一度報告したら以降の報告を送らないゲートで明示的に止める。
  - 併せて、起動失敗で終わったときにログへ `Windows サービスを停止しました` を出さなくなった（起動できていないのに「停止しました」と出るのは誤解を招くため）。**このログ行を「サービスが終了した」の目印にしている運用があれば影響する**。正常な停止時には従来どおり出力される。

### 既知の制限（アルファ）

alpha.3 と同じ（実 DB 検証 S7 未実施、`admin` スコープ API キーはサーバー全権、サイドカーはループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提、OPC UA #201 / SQL Server / SLMP イベント PUSH #258 は未実装）。

## v0.2.0-alpha.3 — 2026-09-07（アルファ）

第 3 アルファ。`v0.2.0-alpha.2` 以降の 10 コミット分。**一体インストーラ（シェル・Hub・elev・サイドカー同梱）**を完成させ、**VC++ 再頒布可能パッケージの前提を排除**した。評価用であり、実 DB 検証（S7）・72h soak・実機サインオフ（#210）等のリリースゲートが未完了なのは alpha.2 と同じ。

### 一体インストーラ（[docs/banto-hub-installer-design.md](docs/banto-hub-installer-design.md)）

- **NSIS インストーラ 1 本に 4 exe を同梱**（#322、I1）: `banto-hub-shell.exe`（main）/ `banto-hub.exe` / `banto-hub-elev.exe` / `banto-hub-sink.exe` を `C:\Program Files\BantoHub\`（PerMachine）に配置。WebView2 は `EmbedBootstrapper`。PREINSTALL で稼働中のサービス（`BantoHubSink` → `BantoHub`）とシェルを停止し、POSTINSTALL で両サービスの登録・Operators への ACL 付与・ProgramData 作成・停止していたサービスの再開まで行う（**収集は勝手に始めない**方針は継続）。
- **サイドカー設定の探索順**（#321、I2）: `BANTO_HUB_SINK_CONFIG` → `%ProgramData%\BantoHub\` → exe 隣。`banto-hub-sink.toml.example` を同梱。
- **リリースビルドの一本化**（#323、I3）: `scripts/build-release.ps1` で UI ビルド → 4 exe → インストーラ生成 → リリース名へのリネームと `SHA256SUMS.txt` 生成までを 1 コマンドに。
- **Windows 実機検証**（#326、I4）: 新規インストール / 稼働中の上書き / Hub 単体インストーラからの更新 / アンインストールが設計どおり動くことを確認（設計 §8）。
- 追従修正: プロファイルロック保持中の `sc start BantoHub` が `START_PENDING` で固まる問題を即時失敗に変更（#327）。サイドカーのサービスログを `%ProgramData%` へ移し、silent インストール時のデスクトップショートカットを削除（#328）。

### VC++ ランタイム前提の排除（設計 §4.7、決定 11）

- MSVC 向けビルドで **C ランタイムと UCRT を静的リンク**（`.cargo/config.toml` の `+crt-static`）。4 exe から `VCRUNTIME140.dll` / `VCRUNTIME140_1.dll` / `api-ms-win-crt-*.dll` のインポートが消え、**Visual C++ 再頒布可能パッケージが入っていない PC でも動く**。
- 主目的は **exe 単体配布**を救うこと。インストーラ経由なら redist を同梱すれば済むが、単体 exe にはインストーラが無く、利用者（オフライン工場 PC を想定）が自力で導入する手段を持たない。
- 併せて Windows 8.1 / 7 での KB2999226（Universal CRT 更新）前提も消える。代償として vcruntime の更新は Windows Update ではなく再ビルド・再配布で届けることになる。サイズ増は 1 exe あたり +110〜145 KB。

### 配布物（Windows x86_64、ローカルビルド）

- `BantoHub_<ver>_x64-setup.exe` — NSIS インストーラ。**alpha.2 の「Hub 本体のみ」から変わり、シェル・Hub・elev・サイドカーの 4 exe を同梱**してサービス登録まで行う。
- `banto-hub-<ver>-windows-x86_64.exe` / `banto-hub-shell-<ver>-windows-x86_64.exe` / `banto-hub-elev-<ver>-windows-x86_64.exe` / `banto-hub-sink-<ver>-windows-x86_64.exe` — 単体配布（サービス化しない評価用途向け、決定 9 で継続）。
- `SHA256SUMS.txt`。
- **前提ランタイム**: VC++ 再頒布可能パッケージは不要。WebView2 のみ、未導入の PC ではインストーラが取得しに行く（単体 exe でシェルを使う場合は WebView2 が要る）。
- Tauri バンドル（`tauri.conf.json`）の版数は数値制約のため `0.2.0`（プレリリース識別子なし）。

### 既知の制限（アルファ）

- alpha.2 の既知の制限はそのまま残る（**外部 DB 連携の実 DB 検証 S7 未実施**、`admin` スコープ API キーはサーバー全権、サイドカーは DB パスワードを平文で受け取るためループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提）。
- 未実装: OPC UA Server（#201）、SQL Server（設計 §6-2、第 2 段）、SLMP イベント PUSH（#258）。

## v0.2.0-alpha.2 — 2026-09-07（アルファ）

第 2 アルファ。`v0.2.0-alpha.1` 以降の 36 コミット分。**外部 DB 連携（#228 DB Source / #229 DB Sink）を実装**し、**デスクトップシェル（Tauri）を配布物に含めた**。評価用であり、実 DB 検証（S7）・72h soak・実機サインオフ（#210）等のリリースゲートは未完了。

### 外部 DB 連携（[docs/banto-hub-external-db-design.md](docs/banto-hub-external-db-design.md)）

- **DB 接続**（`protocol = postgres`）: 既存の接続→グループ→タグの 3 階層に PostgreSQL 接続を追加。パスワードは応答に出さず `passwordSet` のみ、更新は省略で保持・空文字で消去。接続テスト（`SELECT version()`）と MCP `test_saved_connection`。
- **DB Source**（Hub 内）: グループの SQL（`querySql`、1 グループ = 1 SELECT）と `db` タグ（address = 結果列名、数値 / bool / 日時、読み取り専用）。describe + cast 方式で列型を吸収、Quality 変換（NULL / 0 行 / クエリ失敗 = Stale → 2 回で Bad / 接続断 = バックオフ）、収集の開始 / 停止に連動、task の異常終了は supervisor が再生成。UI（Drawer の SQL 入力・列名候補・ツリーの SQL バッジ・状態画面の dbSource 節）、CSV（`tagKind=db` を既存列で受理）、config パッケージ。
- **DB Sink**（別プロセスのサイドカー `apps/banto-hub-sink`）: Hub 側に sink group の設定（`/api/sink/groups`、pending queue に載らず即時適用）と `GET /api/sink/config`（`admin` + `read` の API キー、ループバック前提）/ `PUT /api/sink/status`。サイドカーは banto-tagclient SDK で購読し、long 形式（`ts, tag_id, external_name, value, quality`）へバッチ INSERT（上限付きキュー、at-least-once、1s→30s バックオフ、テーブル検査と推奨 DDL の表示、DDL は発行しない）。Windows サービス `BantoHubSink`（`install` / `grant-service-acl` は同梱の `banto-hub-elev` で Operators の ACE を付与）。UI（sink 画面・推奨 DDL・API キーのプリセット・状態画面の DB Sink 節）とデスクトップシェルの**サービス一覧**（Hub / Sink の SCM 状態と起動停止）。
- **MCP**: sink group 管理 5 ツールを追加し **計 37 ツール**（[docs/banto-hub-mcp-reference.md](docs/banto-hub-mcp-reference.md)）。
- **CI**: ubuntu の PostgreSQL サービスコンテナで Source / Sink の統合テストを常時実行。
- 検証手順: [docs/external-db-test-2026-09.md](docs/external-db-test-2026-09.md)（S3 の手動 smoke A-1〜A-14、S7 の B-1〜B-13）。

### その他

- Rust toolchain を 1.94.1 → **1.98.1** に更新（#294）。sysinfo 0.39、vite-plugin-svelte 7、eslint 10.9 ほか dependabot 8 件。
- banto-tagclient SDK（#123）は機能・実 Hub / LAN 検証・配布サイズ・`v0.1.0` 固定まで完了しクローズ。設計文書 §4.5 に実機で判明した挙動（on-change 配信・書き込み直後の旧値）を記録。
- `Cargo.lock` を版数に同期（#292）。sink group 更新の SQLite `database is locked` 競合を `BEGIN IMMEDIATE` で修正（#309）。

### 配布物（Windows x86_64、ローカルビルド）

- `banto-hub-<ver>-windows-x86_64.exe` — Hub 本体（UI 埋め込み）。
- `banto-hub-shell-<ver>-windows-x86_64.exe` — **デスクトップシェル（Tauri、UI 埋め込み）**。同じディレクトリに `banto-hub-elev.exe` を置く。
- `banto-hub-elev-<ver>-windows-x86_64.exe` — UAC ヘルパ。
- `banto-hub-sink-<ver>-windows-x86_64.exe` — DB Sink サイドカー。
- `BantoHub_<ver>_x64-setup.exe` — NSIS インストーラ（**Hub 本体のみ**。シェル / サイドカーは未同梱、T17 §2.3 のとおり）。
- `SHA256SUMS.txt`。
- Tauri バンドル（`tauri.conf.json`）の版数は数値制約のため `0.2.0`（プレリリース識別子なし）。

### 既知の制限（アルファ）

- **外部 DB 連携の実 DB 検証（S7、別マシン PostgreSQL・24h）は未実施**。シェルのサービス一覧からの Sink の起動停止も実サービス未検証。
- `admin` スコープの API キーはサーバー全権。サイドカーは DB パスワードを平文で受け取るため Hub と同一マシンのループバック運用が前提。
- 72h soak・実機サインオフ（#210）、Windows 実機往復・性能ハーネス（#211）は未実施。通信は平文 + 閉域 LAN 前提。
- 未実装: OPC UA Server（#201）、SQL Server（設計 §6-2、第 2 段）、SLMP イベント PUSH（#258）。

## v0.2.0-alpha.1 — 2026-09-06（アルファ）

初のアルファ評価版。**banto-hub（タグサーバー）**を中心に、MELSEC SLMP / Modbus TCP からの収集・書き込みと多様な外部インターフェースを、**実機検証済み**で提供する。**評価用**であり、72h soak・実機サインオフ（#210）等のリリースゲートは未完了。`v0.1.0`（2026-09-02、最初のリリースタグ）以降の 46 コミット分。

### banto-hub（タグサーバー）

- **収集**: MELSEC SLMP / Modbus TCP ドライバ（いずれも実機 R08ENCPU で検証済み）。broker 抽象化で read/write が1セッションを共有。
- **データ型**: i16 / u16 / i32 / u32 / f32 / bit、文字列（UTF-8 / Shift-JIS）、ワードデバイスのビット（`Dxxx.0`〜`.F`、16進）。
- **書き込み**: 単票・レシピ一括（原子的な事前ゲート＝1件 NG なら無書込）。安全ゲート（writable / スコープ / レート制限 / 値変換 / write-control）。
- **タグ登録 UX（T18）**: グリッド編集・TSV 貼付・連続登録・CSV 入出力・接続/グループ Drawer とツリー。
- **運転（T16/T17）**: デスクトップシェル＋Windows サービスモード、profile 排他、Desktop↔Service 切替。試運転モードとロックダウン（tag-server-design.md §5.6）。
- **UI/UX 群（T19、UX-30〜48）**。
- **外部インターフェース**: REST / WebSocket / MQTT / gRPC / **MCP**。
  - **MCP**: データ面（`list_tags` / `read_tag_values` / `read_tag_now` / `get_server_status` / `write_tag_value` / `write_recipe`）＋**構成補助（管理面）ツール**（接続 / グループ / タグ CRUD・設定 gRPC/MQTT/retention・収集 start/stop・write-control・API キー発行/失効・lock_down）＝**計 31 ツール**（T19 S5 / T20 / T21）。`admin` スコープ・全操作の監査（`origin=mcp`）・不可逆操作の `confirm` で保護。

### 実機検証（2026-09-06）

- **SLMP（R08ENCPU）**: 全データ型の read/write、文字列/レシピ/ビット、MCP 管理面での構築まで（実バグ0）。
- **Modbus TCP（port 502）**: 保持レジスタ/コイル write（FC5/6/16）・保持/コイル/入力 read を、MCP 管理面経由で構築して検証（#219、実バグ0）。

### 権限・安全

- role（viewer / editor / admin）と `require_editor` ゲート、admin 限定ルートの REST テストを整備（#231）。
- API キーのスコープ（`read` / `write:{tag}` / `admin`）と監査。

### 既知の制限（アルファ）

- **72h soak・実機サインオフ（#210）**、Windows 実機往復・性能ハーネス（#211）は未実施（リリースゲート）。
- 通信は v1 = **平文（HTTP / gRPC 平文）＋閉域 LAN 前提**。TLS は未対応。
- **`admin` スコープの API キーはサーバー全権に相当**（構成変更・別 admin キー発行・lock_down）。ロックダウン後も admin スコープで構成可（意図的緩和）。**配布は厳格に管理**すること。
- 未実装: OPC UA Server（#201）、外部DB連携（#228 / #229）、SLMP イベント PUSH（#258）。
- banto-tagclient SDK は実装途上（S4a まで実装、実 Hub/LAN 統合は今後、#123）。
- **GUI バンドル（Tauri / MSI installer）の版数は本タグでは未同期**。本アルファはソース＋hub バイナリの評価用で、配布物（installer）ビルドは別途。

### 関連ドキュメント

- 全体地図: [docs/README.md](docs/README.md)
- 設計の正: [docs/tag-server-design.md](docs/tag-server-design.md)
- 運用ガイド: [docs/banto-hub-operations.md](docs/banto-hub-operations.md)
- MCP インターフェース: [docs/banto-hub-mcp-reference.md](docs/banto-hub-mcp-reference.md)

## v0.1.0 — 2026-09-02

最初のリリースタグ（T0〜T18-6・H 系・banto-tagclient S4a 相当）。
