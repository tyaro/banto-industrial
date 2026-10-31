; ChronoGazer 一体インストーラの NSIS フック（#392 B3-4、
; docs/chronogazer-service-design.md §3.3「インストーラ」「インストーラによる
; シェルの停止」・§4 の B3-4 の実装メモ、apps/chronogazer/README.md「インストーラ」）。
;
; tauri-bundler の NSIS バンドラ（NsisSettings::installer_hooks、
; apps/chronogazer/installer/src/main.rs 参照）が対応する 4 フックのうち、
; NSIS_HOOK_PREINSTALL・NSIS_HOOK_POSTINSTALL・NSIS_HOOK_PREUNINSTALL の 3 つを
; 実装する（NSIS_HOOK_POSTUNINSTALL は使わない。`%ProgramData%\ChronoGazer\` は
; 意図的に残す、下記「PREUNINSTALL」節）。
;
; 作りは Hub のフック（apps/banto-hub/installer/hooks/service-hooks.nsh）と同じ
; （2026-10-31 オーナー決定「基本 Hub と同じで」）。違いは次の 4 つだけ:
;   - サービスは `ChronoGazer` の 1 本（サイドカーは無い）。
;   - シェルは `taskkill` ではなく **`chronogazer.exe --quit-for-update` で正常終了を
;     頼み、プロセスが無くなるまで待つ**（下記「シェルの終了」節。PR #970 の口）。
;   - 旧い利用者単位の版（`cargo tauri build` の currentUser インストーラ）が
;     あれば案内する（O-3）。
;   - tauri-bundler の NSIS テンプレートを当て直して使う（src/main.rs の
;     TEMPLATE_PATCHES、#972 のオーナーレビュー P1・P2）: 既に入っていても
;     「先にアンインストール」の頁を出さず**常にその場で上書き**し（旧い版の
;     アンインストーラが PREUNINSTALL でサービスを解除してしまい、新しい
;     PREINSTALL に「動いていない」と見えて再開も起動種別も失う、を防ぐ。
;     service-install は登録済みなら作り直さないので起動種別は保たれる）、
;     tauri-bundler 自身の `CheckIfAppIsRunning`（強制終了）を使わない
;     （下記「シェルの終了」節）。
;
; main バイナリはシェル `chronogazer.exe`（`${MAINBINARYNAME}`）。記録エンジン
; `chronogazer-server.exe`・昇格ヘルパー `chronogazer-elev.exe` は main ではない
; ので、このフックでは exe 名を直接リテラルで書く（main.rs の `BundleBinary::new`
; と一致させること）。PerMachine でビルドしているので、各フックは常に管理者権限で
; 動く（`chronogazer-elev.exe` は requireAdministrator だが、昇格済みの親から
; 起動されるので UAC の確認は出ない）。
;
; Hub のフックは「どのステップが失敗してもインストーラ本体は中断させない」が、
; ChronoGazer は **失敗を成功扱いにしない**（#972 のオーナーレビュー、4 回分）:
; 何も変えていないうちの確かめで駄目なら `Abort`、サービスの登録・再開が
; 失敗したら案内して `Abort`（インストーラはエラーで終わり、silent では終了
; コード 2）。各節に「その時点で何が変わっているか」「中止で何を戻すか」を書く。
;
; ## 事前の確かめ（ChronoGazerInstallPreflight、Section WebView2 の先頭）
;
; tauri-bundler のテンプレートは Section EarlyChecks → Section WebView2
; （WebView2 ランタイムが無ければ取得して入れる）→ Section Install（フック）の順
; なので、PREINSTALL の確かめは WebView2 の後になり、インストール先が駄目でも
; シェルの終了待ちで [キャンセル] しても WebView2 だけ入ってしまう（#972 の
; Codex の指摘）。そこで src/main.rs の TEMPLATE_PATCHES（P3）が Section WebView2
; の先頭にこのマクロを挿入する。**ここまでは何も変えていない**ので、駄目なら
; `Abort` で止めるだけ（(2) のシェルの終了だけは、頼んだ終了がもう済んでいる
; ことがある。シェルは最終 flush を済ませて終わっているので、利用者が起動し直せば
; よい）。
;
;   (0) インストール先が**既定の場所 `$PROGRAMFILES64\ChronoGazer` そのもの**かを
;       確かめる。違えば案内して `Abort`。`chronogazer-elev service-install` は
;       Program Files の外の `chronogazer-server.exe` を LocalSystem のサービスに
;       登録しない（権限の昇格を防ぐため）ので、外に入れるとサービスの無い
;       インストールが黙ってできてしまう。PerMachine は既定の場所を決めるだけで、
;       ディレクトリの頁や `/D=` で別の場所を選べる（#972 の Codex の指摘）。比べる
;       のは文字列ではなく**本当の置き場**（`..` を畳み、ジャンクションを解いた
;       実パス）で、それが既定の場所の書いたとおりの形と等しいこと。既定の場所か
;       その上のフォルダが再解析ポイントなら断る（#972 のオーナーレビュー
;       （f9d858b）。求まらなければ「違う」）。「Program Files
;       の中ならどこでも」ではなく既定の場所だけにするのは、Program Files の中に
;       一般の利用者が書けるサブフォルダ（別のアプリが緩めたものなど）があると、
;       そこを `/D=` で選べば置いた exe を後から差し替えられるため（#972 の
;       オーナーレビュー（2b9ad63）。既存のフォルダの ACL は、ファイルを置く前に
;       ResetChronoGazerInstallDirAcl が既定へ戻す）。通れば $INSTDIR を `..` を畳んだ形に揃える（#972 の
;       オーナーレビュー（7f86c38）。RequireDefaultInstallDir）。
;   (0b) サービスが登録済みなら、その exe がこの $INSTDIR のものかを確かめる。違えば
;       案内して `Abort`（service-install は登録済みのサービスを作り直さないので、別の
;       フォルダへ入れ直すとサービスが古い exe を指したまま残る。#972 の Codex の指摘）。
;   (0c) この版（HKLM の Uninstall キー）が別のフォルダに入っていて、そこに
;       `chronogazer.exe` がまだあれば、案内して `Abort`（再インストールの頁を
;       出さない（P1）ので、別のフォルダを選ぶと旧い方が残ったまま Uninstall の
;       登録だけ新しい方に移る。サービスが未登録のときは (0b) に掛からないので
;       ここで止める。#972 の総点検）。
;   (1) O-3: 旧い利用者単位の版の案内（下記「旧い利用者単位の版」節）。
;   (2) シェルの終了（下記「シェルの終了」節）。[キャンセル] ならここで中止 -
;       サービスはまだ止めていないので、何も止めずに終わる。
;
; ## PREINSTALL（Section Install の先頭。WebView2 の取得の後）
;
;   (3) シェルの終了の再確認（WebView2 の取得に時間が掛かる間にシェルが起動
;       されたとき用。プロセスが無ければすぐ返る）。[キャンセル] なら `Abort` -
;       まだ何も変えていない。
;   (4) サービス `ChronoGazer` が Running なら停止し、$ChronoGazerWasRunning に
;       "1" を記録（共通部品の StopServiceIfRunning。未登録・停止中は静かに素通り）。
;   (5) サービスが RUNNING／*_PENDING のままなら案内して `Abort`
;       （RequireChronoGazerServiceInactive）。StopServiceIfRunning は 30 秒で
;       止まらなくても案内だけで続ける（Hub の方針）が、止まっていない exe への
;       `File` は NSIS の「使用中」の中止／再試行／無視になり、中止すると
;       「サービスは止めたまま・ファイルは一部だけ新しい」になる。そこでファイルは
;       替えずに止める。自分で止めたのに止まり切らなかったときは、最大 60 秒だけ
;       停止の完了を待ち、STOPPED になれば**続行**する（Abort も再開もしない。
;       $ChronoGazerWasRunning は "1" のまま、POSTINSTALL が入れ終えた後に再開し、
;       失敗なら `.onInstFailed` が再開する）。RUNNING のまま（停止を拒否された）か
;       まだ PENDING なら、$ChronoGazerWasRunning を "0" にして案内し `Abort`
;       （#972 のオーナーレビュー（799c643）P2 の作り直し）。
;   この後、テンプレートの再確認（P2、同じ QuitShellForUpdate。[キャンセル] は
;   止めたサービスを戻してから `Abort`）→ **インストール先の所有者・ACL の既定への
;   戻し**（ResetChronoGazerInstallDirAcl。弱い ACL の既存フォルダに exe を置かない。
;   `icacls /T` ではなく、インストール先そのものと直下の普通のファイルだけを /L で
;   1 つずつ戻し、再解析ポイントやサブフォルダがあれば `Abort`。
;   失敗なら `Abort`、まだファイルは変えていない。#972 のオーナーレビュー（2b9ad63・
;   fd7973e））
;   → `ClearErrors` → `File` でファイルの
;   上書き → **上書きの結果の確かめ**（RequireChronoGazerFilesCopied、P5。
;   #972 のオーナーレビュー 5 回目）。
;
; ## ファイルの上書きの結果（P5、Section Install の `File` の直後）
;
;   `File` が使用中などで書けないと NSIS は中止／再試行／無視の窓を出し、
;   「無視」はそのファイルを飛ばしてエラーフラグを立てるだけで続行する。原本は
;   フラグを見ないので、旧い exe と新しい exe が混ざったまま POSTINSTALL の
;   `service-install`（`ClearErrors` する）を通って成功と報告されていた。そこで:
;   - `AllowSkipFiles off`（P5、インストーラ属性。インストーラ全体に効く）で
;     「無視」を出さない。窓は再試行／中止だけになり、silent では中止になる
;     （`File` の失敗 = `Abort` = `.onInstFailed`）。
;   - それでも `File`・`CreateDirectory` の失敗が成功扱いにならないよう、
;     `File` の直前で `ClearErrors`、直後に `${If} ${Errors}` で確かめ、立っていれば
;     案内して `Abort`（POSTINSTALL には進まない）。
;   中止のとき `.onInstFailed` が止めたサービスを戻す。**一部だけ新しい状態で
;   戻してよい理由**: `File` は書けなければ元のファイルに触らない（開けた時点で
;   上書きする）ので、各 exe は丸ごと旧いか丸ごと新しいかのどちらかで、壊れた
;   exe は無い。サービスは登録済みの `chronogazer-server.exe`（旧いか新しいか）で、
;   上書きの前と同じ設定（登録・ACL は前回の service-install のまま。
;   service-install は登録を作り直さない）で動く。収集を止めたままにする方が
;   記録の欠けになるので、上書きの前の状態（動いていた）に戻す。インストーラは
;   失敗で終わるので、利用者はもう一度実行して揃える（案内に書く）。
;
; ## POSTINSTALL
;
;   (1) `chronogazer-elev.exe service-install`（登録 → `ChronoGazer Operators` →
;       サービスの ACL → データと secrets の ACL。README「昇格ヘルパー」の表）。
;       失敗（非 0、または起動できない）は成功扱いにしない: 問いを出して `Abort`
;       でインストーラをエラーで終える（#972 のオーナーレビュー P1）。ただし
;       上書きの前に動いていたサービスは、失敗の道でも再開を試み（前回の登録・
;       新しいファイルで動く）、再開の成否を問いに書く（#972 のオーナーレビュー
;       （fb82551）: 止めたままだと記録が止まる）。`Abort` の前にフラグを "0" に
;       して .onInstFailed に二重に戻させない。冪等 - 登録済みなら登録は変えない。データのルートが
;       使用中（サービスがまだ止まっていない・コンソール版が動いている）なら、
;       ACL の付け直しだけを飛ばして警告 1 行で 0（#971、Hub と同じ。警告は標準
;       エラー出力だけでインストーラには残らない）。`--install-user-sid` は渡さ
;       ない - インストール利用者は UAC を通した利用者になる（README「インストーラ」
;       の既知の制限、O-2）。
;   (2) PREINSTALL で止めたときだけ `sc start` し、**RUNNING になるまで最大 30 秒
;       待つ**（共通部品の StartServiceIfStoppedAndWait）。`sc start` の失敗・
;       直後の STOPPED・30 秒のタイムアウトは案内して `Abort`（ファイルと登録は
;       入っている。サービスは止まったまま。#972 のオーナーレビュー: 再開の失敗を
;       成功扱いにしない）。新規インストールでは $ChronoGazerWasRunning が未設定
;       なので何も起動しない（収集を勝手に始めない）。
;   ファイアウォールの規則は作らない（LAN 公開は利用者が有効にしたときに
;   シェル／サービスが行う）。
;
; ## .onInstFailed（ChronoGazerOnInstFailed、テンプレートの当て直し P4）
;
;   Section Install が途中で `Abort` した／`File` の「使用中」で中止したときに、
;   PREINSTALL で止めてまだ戻していないサービス（$ChronoGazerWasRunning が "1"）を
;   StartServiceIfStoppedAndWait で戻す（戻せなければ案内するだけ）。自分で
;   `Abort` する道は、戻すか（P2 の再確認の [キャンセル]）、戻さないと決めて
;   "0" にするか（(5)・POSTINSTALL の失敗）のどちらかを先に済ませているので、
;   ここが効くのは `File` の失敗（上書きの直前にシェルが起動された・ウイルス
;   対策のロック。NSIS の窓の [中止] と、P5 の確かめの `Abort` の両方）のとき
;   だけ。silent でも呼ばれる。
;
; ## PREUNINSTALL
;
;   シェルの終了（PREINSTALL と同じ待ち。[キャンセル] でアンインストールを中止）
;   → サービスの停止 → RUNNING／*_PENDING のままなら `Abort`（(5) と同じ）、まで。
;   **登録の解除はここでしない**。Section Uninstall の再確認（下記）の後に
;   次の順で進む（テンプレートの当て直し P2・P6。#972 のオーナーレビュー 5 回目）:
;   (a) **使用中の exe の確かめ**（RequireChronoGazerExesUnlocked）: 3 つの exe の
;       それぞれを `FileOpen ... a`（読み書きで開く。中身は変えずすぐ閉じる）で
;       開けるかを見る。動いている exe はローダーが書込みの共有を許さないので
;       開けない（`Rename` は動いている exe でも通るので判定に使えない。
;       `Delete` も動いている exe には失敗するが、消してから気づくのでは遅い）。
;       1 つでも開けなければ、止めたサービスを戻して案内し `Abort`（まだ何も
;       変更していない）。再確認の後にシェルが起動された・コンソール版の
;       `chronogazer-server.exe` が動いている、が掛かる。
;   (b) UnregisterChronoGazerService が `chronogazer-elev.exe service-uninstall`
;       （無ければ `chronogazer-server.exe uninstall`、それも無ければ登録が残って
;       いれば `sc delete ChronoGazer`）を行う。非 0（または起動できない）で失敗
;       したら止めたサービスを再開し（戻せなければそれも案内）、メッセージを
;       出して `Abort` する（ファイルを消す前に止まる）。
;   (c) テンプレートの `Delete` で 3 つの exe を消す。
;   (d) **消えたことの確かめ**（RequireChronoGazerExesDeleted）: 1 つでも残って
;       いれば（(a) の後に起動された・ウイルス対策のロック）、`uninstall.exe` と
;       「アプリ」の登録（HKLM の Uninstall キー）を消す**前に**止める。
;       サービスの登録は (b) で消えているので、`chronogazer-elev.exe` が残って
;       いれば `service-install` で戻し（起動種別は手動に戻る。自動開始にして
;       いたなら付け直す、と案内）、止めていたサービスを再開する。elev が消えて
;       いる・`service-install` が失敗したときは戻せないので、手順（このインス
;       トーラをもう一度実行して入れ直す）を案内する。どちらも `Abort`。
;       残った exe 以外は消えているので、もう一度アンインストールすると、
;       (a) は無いファイルを飛ばし、(b) は elev が無ければフォールバックする。
;   `%ProgramData%\ChronoGazer\`（DB・時系列データ・Hub のキー・ログ）は
;   **削除しない**（再インストールでデータが戻ることを優先する。Hub と同じ）。
;
; ## 旧い利用者単位の版（O-3、2026-10-31 オーナー決定「案内して続行」）
;
; このインストーラより前の ChronoGazer は `cargo tauri build` の既定
; （NSIS、currentUser）で配られていた。tauri-bundler 2.9.4 の installer.nsi
; （ソース確認済み）は、currentUser では `SetShellVarContext current` のもとで
; `SHCTX` = HKCU に `${UNINSTKEY}` = `Software\Microsoft\Windows\CurrentVersion\
; Uninstall\${PRODUCTNAME}`（= `...\Uninstall\ChronoGazer`、PRODUCTNAME は
; tauri.conf.json と同じ "ChronoGazer"）を書き、`$INSTDIR` は
; `$LOCALAPPDATA\ChronoGazer`。この PerMachine のインストーラは同じ名前のキーを
; HKLM に書くので、テンプレートの「既にインストール済み」の判定（`.onInit` の
; `ReadRegStr SHCTX "${UNINSTKEY}"`）にも MainBinaryName の比較にも旧い版は
; 掛からず、黙って 2 つ並ぶ。そこで PREINSTALL の最初に HKCU の同じキーを読み、
; あれば「旧い版は残る。設定の「アプリ」から ChronoGazer（利用者単位の版）を
; アンインストールする」と案内してから続ける（消しはしない）。silent では
; `/SD IDOK` で止まらない。見るのは昇格したプロセスの HKCU なので、別の管理者の
; 資格情報で UAC を通したときはその管理者の HKCU になり、画面を使う利用者の
; 旧い版は見つからない（受け入れる制限。案内が出ないだけで害は無い）。
;
; ## シェルの終了（O-5、docs/chronogazer-service-design.md §3.3）
;
; サービスが止まっている間はシェル自身が収集エンジンを持っている（デスクトップ
; host）ので、`taskkill /F` では最終 flush と `collection_stopped` を失う。
; そこで `"$INSTDIR\chronogazer.exe" --quit-for-update`（PR #970）を起動する:
; シェルが動いていれば single-instance 経由で引数が渡り、新しいプロセスは
; すぐ終わり、動いているシェルが確認ダイアログを出さずに後始末を通して終わる
; （最悪 65 秒 = `lifecycle::QUIT_FOR_UPDATE_WORST_CASE`）。動いていなければ
; 新しいプロセスは何もせず 0 で終わる。どちらでも `ExecWait` はすぐ返る。
; その後 `tasklist` で `chronogazer.exe` のプロセスが 1 つも無くなるまで
; 1 秒おきに最大 90 秒（65 秒 + 余裕 25 秒）待つ。上限を超えても**強制終了は
; しない**（DB の復旧中の保留は上限を持たない・後始末が済まなかった知らせの
; 窓は利用者が OK を押すまで閉じない、のどちらも殺すと最後の記録を失う）。
; 代わりに [再試行]（もう 90 秒待つ）／[キャンセル]（`Abort` で中止。トレイの
; 「終了」で閉じてからもう一度、と案内）を聞く。silent では `/SD IDCANCEL` で
; 中止する。`$INSTDIR\chronogazer.exe` が無い（初めてのインストールで、別の
; 場所の ChronoGazer - 旧い利用者単位の版や開発のビルド - が動いている）とき
; は頼む口が無いので、待たずにすぐ同じ問いを出す。
;
; **tauri-bundler 自身の `CheckIfAppIsRunning`（nsis/utils.nsh）は使わない**
; （#972 のオーナーレビュー P2）。原本のテンプレートは PREINSTALL・PREUNINSTALL の
; 直後に `!insertmacro CheckIfAppIsRunning` を置き、`chronogazer.exe` が残って
; いれば silent では問答無用で、UI では OK の後に `KillProcess` する。フックの
; 待ちの後にシェルが起動されただけでそこに入るので、src/main.rs の
; TEMPLATE_PATCHES がその挿入を **同じ `QuitShellForUpdate` の再確認**に置き換え
; ている（プロセスが無ければすぐ返る。残っていれば同じ頼んで待つ・再試行／中止）。
; つまり Section Install では「事前の確かめの待ち（WebView2 の前）→ PREINSTALL の
; 再確認 → サービスの停止 → 再確認 → ファイルの上書き」の順になり、最後の再確認の
; [キャンセル] は止めたサービスを戻してから `Abort` する（`restart_service_on_cancel`
; = 1。戻せなければそれも案内する）。Section Uninstall も同じ順
; （PREUNINSTALL = シェルの終了とサービスの停止 → 再確認 → サービスの登録解除 →
; ファイルの削除）で、再確認の [キャンセル] は止めたサービスを戻して `Abort`
; する（まだ何も変更していない）。
; 残る隙間は「再確認の後・ファイルの上書きの前にシェルが起動される」の一瞬だけで、
; そのときは NSIS 自身の「ファイルが使用中」の中止／再試行の窓になる
; （`AllowSkipFiles off` で「無視」は無い。強制終了にはならない。上の「ファイルの
; 上書きの結果」節）。アンインストールでは上の PREUNINSTALL の (a)・(d) が掛かる。
;
; 当て直したテンプレートは `QuitShellForUpdate`・`ChronoGazerInstallPreflight`・
; `UnregisterChronoGazerService`・`ChronoGazerOnInstFailed`・
; `RequireChronoGazerFilesCopied`・`RequireChronoGazerExesUnlocked`・
; `RequireChronoGazerExesDeleted` を `!insertmacro` する
; ので、マクロの名前・引数の並びを変えるときは src/main.rs の TEMPLATE_PATCHES も
; 一緒に直す（試験 `patch_template_applies_*` が名前と並びを見る）。
;
; プロセスの有無は共通部品の TerminateProcessIfRunning と同じ
; `cmd /c tasklist /FI "IMAGENAME eq ..." | findstr ...` の終了コードで見る
; （新しい NSIS プラグインは足さない。昇格しているので全セッションのプロセスが
; 見える = 別の利用者のセッションで動いているシェルも待つ対象になる。そのシェル
; には single-instance の引数は届かない（セッション毎）ので、上限の後の問いで
; 利用者に閉じてもらう）。

; --- 共通部品 -----------------------------------------------------------
;
; サービスの停止・止めたサービスの再開は Hub と共通の installer-common/
; service-common.nsh（StopServiceIfRunning・StartServiceIfStoppedAndWait。結果を
; 見ない StartServiceIfStopped は Hub 用でここでは使わない）。製品固有の
; 文言は include の前に定数で渡す。`BANTO_INSTALLER_COMMON_DIR` は、このフックを
; tauri-bundler に渡す前に apps/chronogazer/installer/src/main.rs が先頭へ
; `!define` する。共通部品の TerminateProcessIfRunning（taskkill）はこの
; インストーラでは使わない（上の「シェルの終了」）。
!define SERVICE_COMMON_LOG_PREFIX "chronogazer"
!define SERVICE_COMMON_OPS_DOC "apps/chronogazer/README.md「インストーラ」"
!include "${BANTO_INSTALLER_COMMON_DIR}\service-common.nsh"

; シェルの終了を待つ上限（秒）。`lifecycle::QUIT_FOR_UPDATE_WORST_CASE`（65 秒）
; + 余裕 25 秒。シェル側の予算を変えたらここと設計書 §3.3 も直す。
!define CHRONOGAZER_SHELL_QUIT_WAIT_SECONDS 90
!define CHRONOGAZER_SHELL_EXE "chronogazer.exe"
; 30 秒で止まり切らなかったサービスの停止完了を、Abort の前にさらに待つ上限（秒）。
!define CHRONOGAZER_SERVICE_STOP_EXTRA_WAIT_SECONDS 60
; tauri-bundler の Uninstall キー（`${UNINSTKEY}` と同じ値）。この版は HKLM、旧い
; 利用者単位の版は HKCU に同じ名前で書く。
!define CHRONOGAZER_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ChronoGazer"

Var ChronoGazerWasRunning
Var ShellQuitWaitCounter
Var ServiceStopWaitCounter

; `chronogazer.exe` のプロセスが 1 つでもあれば $0 = "0"、無ければそれ以外。
!macro CheckShellProcess
  nsExec::ExecToStack 'cmd /c tasklist /FI "IMAGENAME eq ${CHRONOGAZER_SHELL_EXE}" | findstr /I /C:"${CHRONOGAZER_SHELL_EXE}" >nul'
  Pop $0
!macroend

; シェルに正常終了を頼み、プロセスが無くなるまで待つ（上の「シェルの終了」節）。
; プロセスが無ければ何もせずすぐ抜ける。
;   `action`: 問いの文言に出す「インストール」／「アンインストール」。
;   `restart_service_on_cancel`: 1 なら [キャンセル] の `Abort` の前に、
;     PREINSTALL で止めたサービスを RestartChronoGazerServiceOrWarn で戻す（Section
;     Install／Uninstall の再確認用。フックの中では止める前なので 0）。
;   `cancel_note`: [キャンセル] の文言に添える「中止したら何が残るか」。
; [キャンセル] では `Abort` で呼び出し元の Section ごと止まる。
; フックの中と、当て直したテンプレート（src/main.rs の TEMPLATE_PATCHES）の
; Section Install／Uninstall の再確認から、同じ Section に 2 回ずつ挿入される
; ので、ラベルは `${__COUNTER__}`（挿入ごとに増える）で一意にする（`__LINE__`
; だと別のファイルからの挿入と同じ行番号になり得る）。
!macro QuitShellForUpdate action restart_service_on_cancel cancel_note
  !define ShellQuitID ${__COUNTER__}

  !insertmacro CheckShellProcess
  ${If} $0 != "0"
    Goto shellquit_done_${ShellQuitID}
  ${EndIf}

  ${If} ${FileExists} "$INSTDIR\${CHRONOGAZER_SHELL_EXE}"
    DetailPrint "chronogazer: デスクトップシェル (${CHRONOGAZER_SHELL_EXE}) に終了を頼んでいます（後始末の終わりを最大 ${CHRONOGAZER_SHELL_QUIT_WAIT_SECONDS} 秒待ちます）..."
    ExecWait '"$INSTDIR\${CHRONOGAZER_SHELL_EXE}" --quit-for-update' $0
    Goto shellquit_wait_${ShellQuitID}
  ${Else}
    DetailPrint "chronogazer: デスクトップシェル (${CHRONOGAZER_SHELL_EXE}) が動いていますが、$INSTDIR に終了を頼める exe が無いので待てません。"
    Goto shellquit_ask_${ShellQuitID}
  ${EndIf}

  shellquit_wait_${ShellQuitID}:
    StrCpy $ShellQuitWaitCounter 0
  shellquit_poll_${ShellQuitID}:
    !insertmacro CheckShellProcess
    ${If} $0 != "0"
      DetailPrint "chronogazer: デスクトップシェルは終了しました。"
      Goto shellquit_done_${ShellQuitID}
    ${EndIf}
    IntOp $ShellQuitWaitCounter $ShellQuitWaitCounter + 1
    ${If} $ShellQuitWaitCounter >= ${CHRONOGAZER_SHELL_QUIT_WAIT_SECONDS}
      DetailPrint "chronogazer: デスクトップシェルの終了待ちが ${CHRONOGAZER_SHELL_QUIT_WAIT_SECONDS} 秒を超えました（強制終了はしません）。"
      Goto shellquit_ask_${ShellQuitID}
    ${EndIf}
    Sleep 1000
    Goto shellquit_poll_${ShellQuitID}

  shellquit_ask_${ShellQuitID}:
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "ChronoGazer のデスクトップシェル (${CHRONOGAZER_SHELL_EXE}) がまだ終了していません。$\r$\n$\r$\nタスクトレイの ChronoGazer から「終了」を選び（後始末の知らせの窓が出ていれば OK を押し）、終了してから [再試行] を押してください（もう ${CHRONOGAZER_SHELL_QUIT_WAIT_SECONDS} 秒待ちます）。$\r$\n[キャンセル] で${action}を中止します（強制終了はしません。${cancel_note}）。" /SD IDCANCEL IDRETRY shellquit_wait_${ShellQuitID}
    DetailPrint "chronogazer: デスクトップシェルが終了しないため${action}を中止しました（${cancel_note}）。タスクトレイの ChronoGazer から「終了」を選んでから、もう一度実行してください。"
    !if "${restart_service_on_cancel}" == "1"
      !insertmacro RestartChronoGazerServiceOrWarn "${action}"
    !endif
    Abort "ChronoGazer のデスクトップシェルが終了していないため、${action}を中止しました。"

  shellquit_done_${ShellQuitID}:
  !undef ShellQuitID
!macroend

; 止めたサービス（$ChronoGazerWasRunning が "1"）を戻し、戻せなければ案内する
; （Abort は呼ぶ側。中止の道で使うので、ここでは止めない）。共通部品が
; $ChronoGazerWasRunning を "0" に戻すので、同じ Section で 2 回呼んでも 2 回目は
; 何もしない。$0・$1 を壊す。
;   `action`: 文言に出す「インストール」／「アンインストール」。
!macro RestartChronoGazerServiceOrWarn action
  !insertmacro StartServiceIfStoppedAndWait "ChronoGazer" $ChronoGazerWasRunning
  ${If} $0 != "0"
    DetailPrint "chronogazer: ${action}の中止の後、止めていた ChronoGazer サービスを再開できませんでした（$0）。管理者権限の PowerShell から sc start ChronoGazer で開始するか、シェルの「運用モード」で開始してください。"
    MessageBox MB_OK|MB_ICONEXCLAMATION "${action}を中止しましたが、その前に止めていた ChronoGazer サービスを再開できませんでした（$0）。$\r$\n$\r$\nサービスの状態を確かめ（sc query ChronoGazer）、管理者権限の PowerShell から sc start ChronoGazer で開始するか、シェルの「運用モード」で開始してください。" /SD IDOK
  ${EndIf}
!macroend

; (5): サービス ChronoGazer が RUNNING か *_PENDING（START／STOP／PAUSE／CONTINUE）の
; ままなら止める（上の「PREINSTALL」(5)）。未登録・STOPPED なら素通り。
; 自分で止めた（$ChronoGazerWasRunning が "1"）のに 30 秒で止まり切らなかったときは、
; 最大 ${CHRONOGAZER_SERVICE_STOP_EXTRA_WAIT_SECONDS} 秒だけ停止の完了を待つ
; （#972 のオーナーレビュー（799c643）P2 の作り直し）:
;   - STOPPED になった: 止まり切ったので**続行**する（Abort も再開もしない。
;     $ChronoGazerWasRunning は "1" のまま、POSTINSTALL が入れ終えた後に再開し、
;     失敗なら .onInstFailed が再開する。アンインストールでも続行が正しい）。
;   - RUNNING のまま（停止を拒否された）: 戻すものは無い。"0" にして案内し Abort。
;   - まだ PENDING: "0" にして案内（sc query が STOPPED になってから開始）し Abort。
; フラグが "1" でなければ（頼めない状態）、従来どおり案内して Abort。
; $0・$1 を壊す（呼び出しの後ろで使わない）。
;   `action`: 文言に出す「インストール」／「アンインストール」。
!macro RequireChronoGazerServiceInactive action
  nsExec::ExecToStack 'cmd /c sc query "ChronoGazer" | findstr /C:"RUNNING" >nul'
  Pop $0
  ${If} $0 != "0"
    nsExec::ExecToStack 'cmd /c sc query "ChronoGazer" | findstr /C:"PENDING" >nul'
    Pop $0
  ${EndIf}
  ${If} $0 == "0"
    DetailPrint "chronogazer: ChronoGazer サービスが止まっていません（RUNNING か PENDING のまま）。"
    StrCpy $1 "nowait"
    ${If} $ChronoGazerWasRunning == "1"
      ; 自分で止めた。停止の完了を待つ。
      DetailPrint "chronogazer: 停止の完了を最大 ${CHRONOGAZER_SERVICE_STOP_EXTRA_WAIT_SECONDS} 秒待ちます..."
      StrCpy $ServiceStopWaitCounter 0
      ${Do}
        nsExec::ExecToStack 'cmd /c sc query "ChronoGazer" | findstr /C:"PENDING" >nul'
        Pop $0
        ${If} $0 != "0"
          ${Break}
        ${EndIf}
        IntOp $ServiceStopWaitCounter $ServiceStopWaitCounter + 1
        ${If} $ServiceStopWaitCounter >= ${CHRONOGAZER_SERVICE_STOP_EXTRA_WAIT_SECONDS}
          ${Break}
        ${EndIf}
        Sleep 1000
      ${Loop}
      ${If} $0 == "0"
        StrCpy $1 "pending"
      ${Else}
        nsExec::ExecToStack 'cmd /c sc query "ChronoGazer" | findstr /C:"STOPPED" >nul'
        Pop $0
        ${If} $0 == "0"
          StrCpy $1 "stopped"
        ${Else}
          StrCpy $1 "running"
        ${EndIf}
      ${EndIf}
    ${EndIf}
    ${If} $1 == "stopped"
      ; 止まり切った。続行する（再開は POSTINSTALL／.onInstFailed が行うので、
      ; $ChronoGazerWasRunning は "1" のまま）。
      DetailPrint "chronogazer: ChronoGazer サービスの停止が完了しました。続行します。"
    ${Else}
      ${If} $1 == "running"
        ; 停止を拒否された。戻すものは無い。
        StrCpy $ChronoGazerWasRunning "0"
        MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスが停止の要求を受け付けず、RUNNING のままです。$\r$\n$\r$\n実行ファイルを入れ替えられないため、${action}を中止しました。ファイルはまだ変更していません。サービスは動いたままです。サービスの状態を確かめ（sc query ChronoGazer）、止めてからもう一度実行してください。" /SD IDOK
      ${ElseIf} $1 == "pending"
        ; 停止の途中のまま。自分で戻す操作は無い。
        StrCpy $ChronoGazerWasRunning "0"
        MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスの停止がまだ終わっていません（停止を頼んで、さらに ${CHRONOGAZER_SERVICE_STOP_EXTRA_WAIT_SECONDS} 秒待ちましたが、PENDING のままです）。$\r$\n$\r$\n実行ファイルを入れ替えられないため、${action}を中止しました。ファイルはまだ変更していません。sc query ChronoGazer で STOPPED になってから、管理者権限の PowerShell から sc start ChronoGazer で開始するか、シェルの「運用モード」で開始してください。" /SD IDOK
      ${Else}
        ; 停止はもう頼んである（または頼めない状態）。戻す操作は無い。
        StrCpy $ChronoGazerWasRunning "0"
        MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスがまだ止まっていません（RUNNING か PENDING のままです）。$\r$\n$\r$\n実行ファイルを入れ替えられないため、${action}を中止しました。ファイルはまだ変更していません。サービスの状態を確かめ（sc query ChronoGazer）、止まってからもう一度実行してください。" /SD IDOK
      ${EndIf}
      Abort "ChronoGazer サービスが止まっていないため、${action}を中止しました。"
    ${EndIf}
  ${EndIf}
!macroend

; #972 のオーナーレビュー（2b9ad63）: インストール先が Program Files の中で既定の
; 場所だけに限っても、管理者が弱い ACL で先に作った `C:\Program Files\ChronoGazer`
; が残っていると、一般の利用者が LocalSystem のサービスの exe を差し替えられる。
; そこで、ファイルを置く前（テンプレートの当て直しが `ClearErrors` の前に挿入）に、
; 所有者を BUILTIN\Administrators（S-1-5-32-544）にし、明示の ACE を捨てて Program
; Files から継承する ACL だけにする。どれかが駄目なら案内して `Abort`
; （まだファイルは変えていない。止めたサービスは .onInstFailed が戻す）。
;
; **`icacls /T` は使わない**（#972 のオーナーレビュー（fd7973e）P1）: /T は子を再帰で
; 辿り、/L を付けないとリンクそのものではなく**リンク先**を処理する。事前の確かめ
; （RequireDefaultInstallDir）が見たのはインストール先とその上のフォルダだけなので、
; 弱い ACL の既存フォルダに一般の利用者がシンボリックリンクやジャンクションを置いて
; いれば、昇格した icacls がインストール先の**外**の所有者・ACL を書き換えていた。
; そこで ACL を触る対象を「確かめ済みのインストール先そのもの」と「その直下の普通の
; ファイル」に限る:
;   (a) インストール先そのものが再解析ポイントでないことをもう一度確かめ
;       （GetFileAttributesW の FILE_ATTRIBUTE_REPARSE_POINT = 0x400。上のフォルダは
;       Program Files で一般の利用者は書けないので、事前の確かめのままとみなす）、
;       `/setowner` と `/reset` を **/L（リンクを解かない）付き・/T なし**で掛ける。
;       これで直下に新しい項目を作る・名前を変える（FILE_ADD_FILE・
;       FILE_ADD_SUBDIRECTORY）のは管理者だけになる（継承する Program Files の ACL は
;       Users に読み取りと実行だけ）。
;   (b) 直下の項目を FindFirst/FindNext で 1 つずつ見る（`.`・`..` は飛ばす）:
;       - 再解析ポイント（属性を読めないものも含む）→ 案内して `Abort`。icacls が
;         インストール先の外に触らないようにするため。
;       - フォルダ → 案内して `Abort`。このインストーラはインストール先に
;         サブフォルダを作らない（tauri-bundler のテンプレートは main・外部バイナリの
;         `File` と `uninstall.exe` の書き込みだけで、resources は無い）ので、あれば
;         利用者か別のプログラムが置いた物。再帰で辿る代わりに、消してから（または
;         アンインストールしてから）やり直してもらう。
;       - 普通のファイル → `/setowner` と `/reset` を /L 付き・/T なしで掛ける。
;         残っているファイルも既定の ACL に戻すのは、chronogazer-server.exe の隣に
;         利用者が書ける DLL が残ると、LocalSystem のサービスが DLL の検索順で
;         それを読み込みうるため（置き換えでなく「残ったまま」でも危ない）。
;   (a) の後は一般の利用者が直下に項目を足せないので、(b) で見た項目が icacls までの
;   間に別の物へ差し替わることはない（自分の項目を消すことはできるが、足せない）。
;
; $0〜$5 を壊す（$2 は FindFirst のハンドル。中止の道は全部 FindClose してから `Abort`）。
!macro ResetChronoGazerInstallDirAcl
  ; (a) インストール先そのもの。
  StrCpy $4 "$INSTDIR"
  System::Call 'kernel32::GetFileAttributesW(w r4) i .r5'
  IntOp $0 $5 & 1024
  ${If} $5 == -1
  ${OrIf} $0 != 0
    !insertmacro AbortChronoGazerInstallDirAcl "インストール先そのものが再解析ポイント（ジャンクションやリンク）になっているか、属性を読めませんでした。"
  ${EndIf}
  !insertmacro IcaclsResetChronoGazerEntry
  ${If} $0 != "0"
    DetailPrint "chronogazer: icacls の出力: $1"
    !insertmacro AbortChronoGazerInstallDirAcl "icacls が失敗しました（終了コード $0）。"
  ${EndIf}
  ; (b) 直下の項目。
  FindFirst $2 $3 "$INSTDIR\*.*"
  ${If} $2 == ""
    !insertmacro AbortChronoGazerInstallDirAcl "インストール先の中身を列挙できませんでした。"
  ${EndIf}
  ${Do}
    ${If} $3 == ""
      ${ExitDo}
    ${EndIf}
    ${If} $3 != "."
    ${AndIf} $3 != ".."
      StrCpy $4 "$INSTDIR\$3"
      System::Call 'kernel32::GetFileAttributesW(w r4) i .r5'
      IntOp $0 $5 & 1024
      IntOp $1 $5 & 16
      ${If} $5 == -1
      ${OrIf} $0 != 0
        FindClose $2
        !insertmacro AbortChronoGazerInstallDirAcl "インストール先の中に再解析ポイント（ジャンクションやリンク）があるか、属性を読めませんでした。"
      ${ElseIf} $1 != 0
        FindClose $2
        !insertmacro AbortChronoGazerInstallDirAcl "インストール先の中にサブフォルダがあります。ChronoGazer のインストーラはサブフォルダを作らないので、ChronoGazer の物ではありません。"
      ${EndIf}
      !insertmacro IcaclsResetChronoGazerEntry
      ${If} $0 != "0"
        DetailPrint "chronogazer: icacls の出力: $1"
        FindClose $2
        !insertmacro AbortChronoGazerInstallDirAcl "icacls が失敗しました（終了コード $0）。"
      ${EndIf}
    ${EndIf}
    FindNext $2 $3
  ${Loop}
  FindClose $2
!macroend

; ResetChronoGazerInstallDirAcl の 1 項目分: $4 のパス（インストール先そのものか、
; その直下の普通のファイル）の所有者を Administrators にし、明示の ACE を捨てる。
; /L でリンクを解かず、/T を付けず（その項目だけ）。$0 に終了コード、$1 に出力。
!macro IcaclsResetChronoGazerEntry
  nsExec::ExecToStack '"$SYSDIR\icacls.exe" "$4" /setowner *S-1-5-32-544 /L /C /Q'
  Pop $0
  Pop $1
  ${If} $0 == "0"
    nsExec::ExecToStack '"$SYSDIR\icacls.exe" "$4" /reset /L /C /Q'
    Pop $0
    Pop $1
  ${EndIf}
!macroend

; ResetChronoGazerInstallDirAcl の中止: $4 の項目について `reason` を案内して `Abort`。
; まだファイルは変えていない。止めたサービスは .onInstFailed が戻す
; （$ChronoGazerWasRunning はそのまま）。呼ぶ前に FindClose を済ませること。
!macro AbortChronoGazerInstallDirAcl reason
  DetailPrint "chronogazer: インストール先 $INSTDIR の所有者・ACL を既定に戻せませんでした。${reason}（対象: $4）"
  MessageBox MB_OK|MB_ICONSTOP "インストール先 $INSTDIR の所有者・アクセス権を既定に戻せませんでした。$\r$\n${reason}$\r$\n対象: $4$\r$\n$\r$\n既存のフォルダのアクセス権が弱いまま実行ファイルを置かないよう、インストールを中止しました。ファイルはまだ変更していません。上書き前に止めた ChronoGazer サービスは戻します。$\r$\n$\r$\n対象のリンクやサブフォルダを取り除く（または先にアンインストールする）か、フォルダを使用しているプログラムを終了するか、管理者権限で $INSTDIR の所有者とアクセス権を確かめてから、もう一度実行してください。" /SD IDOK
  Abort "インストール先の所有者・ACL を既定に戻せなかったため、インストールを中止しました。"
!macroend

; P5: ファイルの上書きの結果の確かめ（上の「ファイルの上書きの結果」節。テンプレートの
; 当て直しが Section Install の `File` の直後に挿入する。直前の `ClearErrors` も
; 当て直しが置く）。エラーフラグが立っていれば案内して `Abort`。止めたサービスは
; `.onInstFailed` が戻す（$ChronoGazerWasRunning はそのまま）。
!macro RequireChronoGazerFilesCopied
  ${If} ${Errors}
    DetailPrint "chronogazer: ファイルの上書きに失敗したものがあります。旧い実行ファイルと新しい実行ファイルが混ざらないよう、インストールを中止します。上書き前に止めた ChronoGazer サービスは戻します。"
    MessageBox MB_OK|MB_ICONSTOP "ファイルの上書きに失敗したものがあります（使用中か、書き込めませんでした）。$\r$\n$\r$\n一部のファイルだけ新しい状態なので、インストールを失敗として終えます。上書き前に止めていた ChronoGazer サービスは戻します（各ファイルは丸ごと旧いか丸ごと新しいかのどちらかで、壊れてはいません）。$\r$\n$\r$\nChronoGazer のデスクトップシェルとコンソール版の chronogazer-server.exe を終了してから、このインストーラをもう一度実行してください。" /SD IDOK
    Abort "ファイルの上書きに失敗したため、インストールを失敗として終えました。"
  ${EndIf}
!macroend

; 使用中の exe の確かめの 1 つ分（上の PREUNINSTALL の (a)）。あって読み書きで
; 開けなければ $1 に名前を足す。`FileOpen ... a` は無いファイルを作るので、
; 先に有無を見る。開けたら中身を変えずに閉じる。$0 を壊す。
!macro CheckChronoGazerExeUnlocked exe_name
  ${If} ${FileExists} "$INSTDIR\${exe_name}"
    ClearErrors
    FileOpen $0 "$INSTDIR\${exe_name}" a
    ${If} ${Errors}
      StrCpy $1 "$1 ${exe_name}"
    ${Else}
      FileClose $0
    ${EndIf}
  ${EndIf}
!macroend

; P6 (a): 3 つの exe のどれかが使用中なら、止めたサービスを戻して `Abort`
; （まだ何も変更していない。上の PREUNINSTALL の (a)）。$0〜$2 を壊す。
!macro RequireChronoGazerExesUnlocked
  StrCpy $1 ""
  !insertmacro CheckChronoGazerExeUnlocked "${CHRONOGAZER_SHELL_EXE}"
  !insertmacro CheckChronoGazerExeUnlocked "chronogazer-server.exe"
  !insertmacro CheckChronoGazerExeUnlocked "chronogazer-elev.exe"
  ${If} $1 != ""
    ; 名前の並びを $2 に控える（RestartChronoGazerServiceOrWarn が $0・$1 を使うため）。
    StrCpy $2 $1
    DetailPrint "chronogazer: 使用中の実行ファイルがあります（$2）。消せないため、アンインストールを中止します（まだ何も変更していません）。"
    !insertmacro RestartChronoGazerServiceOrWarn "アンインストール"
    MessageBox MB_OK|MB_ICONSTOP "使用中の実行ファイルがあります（$2）。$\r$\n$\r$\n消せないため、アンインストールを中止しました。まだ何も変更していません（止めたサービスは再開を試みました）。$\r$\n$\r$\nChronoGazer のデスクトップシェル（タスクトレイの「終了」）とコンソール版の chronogazer-server.exe を終了してから、もう一度アンインストールしてください。" /SD IDOK
    Abort "使用中の実行ファイルがあるため、アンインストールを中止しました。"
  ${EndIf}
!macroend

; 消えたことの確かめの 1 つ分（上の PREUNINSTALL の (d)）。残っていれば $2 に名前を足す。
!macro CheckChronoGazerExeDeleted exe_name
  ${If} ${FileExists} "$INSTDIR\${exe_name}"
    StrCpy $2 "$2 ${exe_name}"
  ${EndIf}
!macroend

; P6 (d): テンプレートの `Delete` の後に exe が残っていれば、`uninstall.exe` と
; 「アプリ」の登録を消す前に止める（上の PREUNINSTALL の (d)）。サービスの登録は
; もう解除されているので、elev が残っていれば `service-install` で戻してから
; 止めていたサービスを再開する。戻せなければ手順を案内する。どちらも `Abort`。
; $0〜$3 を壊す。
!macro RequireChronoGazerExesDeleted
  StrCpy $2 ""
  !insertmacro CheckChronoGazerExeDeleted "${CHRONOGAZER_SHELL_EXE}"
  !insertmacro CheckChronoGazerExeDeleted "chronogazer-server.exe"
  !insertmacro CheckChronoGazerExeDeleted "chronogazer-elev.exe"
  ${If} $2 != ""
    DetailPrint "chronogazer: 実行ファイルを消せませんでした（$2）。uninstall.exe と「アプリ」の登録は残したまま、アンインストールを中止します。"
    ${If} ${FileExists} "$INSTDIR\chronogazer-elev.exe"
      DetailPrint "chronogazer: 解除した ChronoGazer サービスの登録を chronogazer-elev.exe service-install で戻します..."
      ClearErrors
      ExecWait '"$INSTDIR\chronogazer-elev.exe" service-install' $0
      ${If} ${Errors}
        StrCpy $0 "exec-error"
      ${EndIf}
      ${If} $0 == 0
        !insertmacro RestartChronoGazerServiceOrWarn "アンインストール"
        MessageBox MB_OK|MB_ICONSTOP "実行ファイルを消せませんでした（$2。使用中か、消せませんでした）。$\r$\n$\r$\nアンインストールを中止しました。uninstall.exe と「アプリ」の登録は残しています。解除していた ChronoGazer サービスの登録は戻しました（起動種別は手動に戻ります。自動開始にしていたなら、シェルの「運用モード」か chronogazer-elev.exe autostart-enable で付け直してください）。止めていたサービスは再開を試みました。$\r$\n$\r$\nChronoGazer のデスクトップシェルとコンソール版の chronogazer-server.exe を終了してから、もう一度アンインストールしてください。" /SD IDOK
      ${Else}
        StrCpy $3 $0
        StrCpy $ChronoGazerWasRunning "0"
        DetailPrint "chronogazer: ChronoGazer サービスの登録を戻せませんでした（終了コード $3）。このインストーラをもう一度実行して入れ直してください。"
        MessageBox MB_OK|MB_ICONSTOP "実行ファイルを消せませんでした（$2）。$\r$\n$\r$\nアンインストールを中止しました。uninstall.exe と「アプリ」の登録は残していますが、解除した ChronoGazer サービスの登録を戻せませんでした（chronogazer-elev.exe service-install の終了コード $3）。$\r$\n$\r$\nChronoGazer のデスクトップシェルとコンソール版の chronogazer-server.exe を終了してから、このインストーラをもう一度実行して入れ直す（サービスの登録も戻ります）か、もう一度アンインストールしてください。" /SD IDOK
      ${EndIf}
    ${Else}
      StrCpy $ChronoGazerWasRunning "0"
      DetailPrint "chronogazer: chronogazer-elev.exe が消えているため、ChronoGazer サービスの登録を戻せません。このインストーラをもう一度実行して入れ直してください。"
      MessageBox MB_OK|MB_ICONSTOP "実行ファイルを消せませんでした（$2）。$\r$\n$\r$\nアンインストールを中止しました。uninstall.exe と「アプリ」の登録は残していますが、chronogazer-elev.exe が消えているため、解除した ChronoGazer サービスの登録は戻せません。$\r$\n$\r$\nChronoGazer のデスクトップシェルとコンソール版の chronogazer-server.exe を終了してから、このインストーラをもう一度実行して入れ直す（サービスの登録も戻ります）か、もう一度アンインストールしてください。" /SD IDOK
    ${EndIf}
    Abort "実行ファイルを消せなかったため、アンインストールを中止しました。"
  ${EndIf}
!macroend

; 事前の確かめ (0c): この版の Uninstall キー（HKLM）の InstallLocation が $INSTDIR と
; 違い、そこに chronogazer.exe がまだあれば止める（上の「事前の確かめ」(0c)）。
; InstallLocation はテンプレートが `"$INSTDIR"` と引用符付きで書くので外して比べる。
; NSIS の `==` は大文字小文字を区別しない。
!macro RequirePreviousInstallInInstallDir
  ReadRegStr $0 HKLM "${CHRONOGAZER_UNINSTKEY}" "InstallLocation"
  ${If} $0 != ""
    StrCpy $1 $0 1
    ${If} $1 == '"'
      StrCpy $0 $0 "" 1
      StrCpy $0 $0 -1
    ${EndIf}
    ${If} $0 != "$INSTDIR"
    ${AndIf} ${FileExists} "$0\${CHRONOGAZER_SHELL_EXE}"
      DetailPrint "chronogazer: ChronoGazer は既に別の場所 ($0) に入っています。インストール先 $INSTDIR と違うため、インストールを中止しました。"
      MessageBox MB_OK|MB_ICONSTOP "ChronoGazer は既に別の場所に入っています。$\r$\n$\r$\n今の場所: $0$\r$\nインストール先: $INSTDIR$\r$\n$\r$\nこのまま入れると、今の場所のファイルが残ったまま「アプリ」の登録だけ新しい場所に移ります。今の場所へインストールし直すか、先に今の ChronoGazer をアンインストールしてください。$\r$\n$\r$\nまだ何も変更していません。" /SD IDOK
      Abort "ChronoGazer が別の場所に入っているため、インストールを中止しました。"
    ${EndIf}
  ${EndIf}
!macroend

; 事前の確かめ (0): $INSTDIR が既定の場所 `$PROGRAMFILES64\ChronoGazer` **そのもの**で
; なければ止める。NSIS の `==` は大文字小文字を区別しない。
;
; 「Program Files（64 ビット・x86）の中ならどこでも」にはしない: Program Files の中に
; 一般の利用者が書けるサブフォルダ（別のアプリが ACL を緩めたフォルダなど）があると、
; そこを `/D=` で選べば、置いた `chronogazer-server.exe` が LocalSystem のサービスに
; 登録されたうえで後から差し替えられる（#972 のオーナーレビュー（2b9ad63））。
; 既定の場所は親（Program Files）から SYSTEM・Administrators・TrustedInstaller だけが
; 書ける ACL を継ぐ。既存のフォルダの ACL は、ファイルを置く前に
; ResetChronoGazerInstallDirAcl が既定へ戻す。elev 側の ACL の監査は Hub と合わせて
; 別の Issue で検討する。
;
; 文字列だけで見ると `/D=C:\Program Files\..\Users\Public\ChronoGazer` や
; Program Files の中のジャンクションが通り、実際の置き場が外でも、ファイルを
; 写した後の service-install まで気付けない（#972 のオーナーレビュー（7f86c38））。
; そこで、ファイルを変える前のここで**本当の置き場**を比べる: $INSTDIR を
; ChronoGazerResolveRealPath（下。`..` の正規化 + ジャンクションを解いた実パス。
; まだ無いフォルダは、ある親まで解いて残りを継ぎ足す）にし、既定の場所は**解かずに**
; ChronoGazerNormalizePath（`..` などを畳むだけ）にして、全体が**書いたとおりに**
; 等しいかを見る。実パスは存在する全ての構成要素を通して解くので、既定の場所
; `C:\Program Files\ChronoGazer` かその上のフォルダがジャンクション／シンボリック
; リンクだと実パスが書いた既定の場所と食い違い、止まる（Program Files 自体が
; 付け替えられていても同じ。両方を解いて比べるとユーザーが書ける場所への
; ジャンクションが通ってしまう。#972 のオーナーレビュー（f9d858b））。実パスが
; 求まらなければ（API の失敗・ネットワークパスなど）「違う」として止める（安全側）。
;
; 通ったときは $INSTDIR を既定の場所の書き方（`$PROGRAMFILES64\ChronoGazer` を
; `..`・`.`・重なった区切りを畳んだ形）に置き換える。後の (0b)
; （サービスの ImagePath との比較）・(0c)（InstallLocation との比較）と、
; テンプレートが書く InstallLocation・elev が登録する exe のパスが同じ表記に
; なるようにするため。このマクロは事前の確かめの先頭に置くこと。
;
; $0〜$2 を壊す（呼び出し側はこの後に自分で入れ直す。前からの $0〜$4 と同じ）。
!macro RequireDefaultInstallDir
  Push "$INSTDIR"
  Call ChronoGazerResolveRealPath
  Pop $0
  Push "$PROGRAMFILES64\ChronoGazer"
  Call ChronoGazerNormalizePath
  Pop $2
  StrCpy $1 0
  ${If} $0 != ""
  ${AndIf} $2 != ""
  ${AndIf} $0 == $2
    StrCpy $1 1
  ${EndIf}
  ${If} $1 != 1
    ${If} $0 == ""
      StrCpy $0 "（求められませんでした）"
    ${EndIf}
    DetailPrint "chronogazer: インストール先 $INSTDIR（実際の場所: $0）は既定の場所 ($PROGRAMFILES64\ChronoGazer) ではありません（既定の場所かその上のフォルダが、別の場所へのジャンクションやリンクになっている場合も含みます）。記録エンジンのサービスを登録できないため、インストールを中止しました。"
    MessageBox MB_OK|MB_ICONSTOP "インストール先 ($INSTDIR) が既定の場所ではありません。$\r$\n実際の場所: $0$\r$\n$\r$\nChronoGazer は既定の場所 ($PROGRAMFILES64\ChronoGazer) にしかインストールできません（既定の場所かその上のフォルダが、別の場所へのジャンクションやリンクになっている場合も含みます）。記録エンジンは、一般の利用者が書き換えられないこの場所に置かないとサービスとして登録できません。インストール先を変えずにインストールし直してください。$\r$\n$\r$\nまだ何も変更していません。" /SD IDOK
    Abort "インストール先が既定の場所ではないため、インストールを中止しました。"
  ${EndIf}
  ${If} $2 != "$INSTDIR"
    DetailPrint "chronogazer: インストール先 $INSTDIR を $2 として扱います。"
    StrCpy $INSTDIR $2
  ${EndIf}
!macroend

; パスを `kernel32::GetFullPathNameW` で正規化する（`..`・`.`・重なった区切りを
; 畳む。ジャンクションは解かない）。末尾の `\` は取る（ルート `C:\` を除く）。
;   入力: スタックの先頭にパス。出力: スタックの先頭に正規化したパス。失敗
;   （API の失敗・${NSIS_MAX_STRLEN} に収まらない・`\\` で始まる UNC／`\\?\`）
;   なら ""。$0〜$3 は保存して戻す。
Function ChronoGazerNormalizePath
  Exch $0
  Push $1
  Push $2
  Push $3
  System::Call 'kernel32::GetFullPathNameW(w r0, i ${NSIS_MAX_STRLEN}, w .r1, p 0) i .r2'
  ${If} $2 <= 0
  ${OrIf} $2 >= ${NSIS_MAX_STRLEN}
    StrCpy $0 ""
  ${Else}
    StrCpy $0 $1
    StrCpy $3 $0 2
    ${If} $3 == "\\"
      StrCpy $0 ""
    ${Else}
      StrLen $2 $0
      StrCpy $3 $0 1 -1
      ${If} $3 == "\"
      ${AndIf} $2 > 3
        StrCpy $0 $0 -1
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Pop $3
  Pop $2
  Pop $1
  Exch $0
FunctionEnd

; パスの本当の置き場（ジャンクション・シンボリックリンク・SUBST を解いた DOS
; パス）を求める。パスそのものはまだ無くてよい: 存在する最も深い親フォルダを
; `CreateFileW`（FILE_FLAG_BACKUP_SEMANTICS）で開いて
; `GetFinalPathNameByHandleW`（VOLUME_NAME_DOS）で実パスにし、無い残りの部分を
; そのまま継ぎ足す。
;   入力: スタックの先頭にパス。出力: スタックの先頭に実パス（末尾の `\` なし）。
;   正規化の失敗・ルートまで遡っても無い・開けない・API の失敗・
;   ${NSIS_MAX_STRLEN} に収まらない・結果が `\\?\UNC\`（ネットワーク）なら ""
;   （呼び出し側は "" を「Program Files の外」として扱う = 安全側に倒す）。
;   $0〜$5 は保存して戻す。
Function ChronoGazerResolveRealPath
  Exch $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  ; $1 = 存在する親（正規化したパスから、無い末尾の部分を 1 つずつ外す）、
  ; $2 = 外した部分（`\a\b` の形で前に積む）、$0 = 結果。
  Push $0
  Call ChronoGazerNormalizePath
  Pop $1
  StrCpy $2 ""
  StrCpy $0 ""
  ; ルート `C:\` は `C:` にしておく（`$1\*.*`・`$1$2` の継ぎ足しで `\` が重ならない
  ; ように）。開くときだけ `C:\` に戻す（下）。
  StrLen $4 $1
  ${If} $4 == 3
    StrCpy $3 $1 1 -1
    ${If} $3 == "\"
      StrCpy $1 $1 -1
    ${EndIf}
  ${EndIf}
  ${If} $1 != ""
    ${Do}
      ${If} ${FileExists} "$1\*.*"
        ${Break}
      ${EndIf}
      ; 最後の `\` を後ろから探す（$3 = 負の位置）。`C:` まで来て無ければ失敗。
      StrLen $4 $1
      ${If} $4 <= 3
        StrCpy $1 ""
        ${Break}
      ${EndIf}
      StrCpy $3 0
      ${Do}
        IntOp $3 $3 - 1
        IntOp $5 0 - $3
        ${If} $5 > $4
          StrCpy $1 ""
          ${Break}
        ${EndIf}
        StrCpy $5 $1 1 $3
        ${If} $5 == "\"
          ${Break}
        ${EndIf}
      ${Loop}
      ${If} $1 == ""
        ${Break}
      ${EndIf}
      StrCpy $5 $1 "" $3
      StrCpy $2 "$5$2"
      StrCpy $1 $1 $3
    ${Loop}
  ${EndIf}
  ${If} $1 != ""
    ; `C:` だけは `C:\`（`C:` は「C: のカレント」を指してしまう）。
    StrLen $4 $1
    ${If} $4 == 2
      StrCpy $1 "$1\"
    ${EndIf}
    ; アクセス 0（属性だけ）・共有 読み|書き|削除 (7)・OPEN_EXISTING (3)・
    ; FILE_FLAG_BACKUP_SEMANTICS (0x02000000 = 33554432。フォルダを開くのに要る)。
    ; 開けなければ INVALID_HANDLE_VALUE で、次の GetFinalPathNameByHandleW が 0 を
    ; 返すので、別に見なくても失敗になる。
    System::Call 'kernel32::CreateFileW(w r1, i 0, i 7, p 0, i 3, i 33554432, p 0) p .r3'
    System::Call 'kernel32::GetFinalPathNameByHandleW(p r3, w .r4, i ${NSIS_MAX_STRLEN}, i 0) i .r5'
    System::Call 'kernel32::CloseHandle(p r3)'
    ${If} $5 > 0
    ${AndIf} $5 < ${NSIS_MAX_STRLEN}
      StrCpy $3 $4 8
      StrCpy $1 $4 4
      ${If} $3 == "\\?\UNC\"
        StrCpy $0 ""
      ${ElseIf} $1 == "\\?\"
        StrCpy $0 $4 "" 4
        StrCpy $3 $0 1 -1
        ${If} $3 == "\"
          StrCpy $0 $0 -1
        ${EndIf}
        StrCpy $0 "$0$2"
      ${Else}
        StrCpy $0 ""
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Exch $0
FunctionEnd

; 事前の確かめ (0b): サービス ChronoGazer が既に登録されていれば、その実行ファイルが
; この $INSTDIR の chronogazer-server.exe でなければ止める。service-install は登録済みの
; サービスを作り直さない（起動種別などを保つため）ので、別のフォルダへ入れ直すと
; サービスは古い exe を指したまま残り、再開すると古い記録エンジンが動く（#972 の
; Codex の指摘）。ImagePath は `"<exe>" run-service` の形（引用符なしも念のため受ける）。
; NSIS の `==` は大文字小文字を区別しない。
!macro RequireRegisteredServiceInInstallDir
  ReadRegStr $0 HKLM "SYSTEM\CurrentControlSet\Services\ChronoGazer" "ImagePath"
  ${If} $0 != ""
    StrCpy $1 0
    StrCpy $2 '"$INSTDIR\chronogazer-server.exe"'
    StrLen $3 $2
    StrCpy $4 $0 $3
    ${If} $4 == $2
      StrCpy $1 1
    ${EndIf}
    StrCpy $2 "$INSTDIR\chronogazer-server.exe "
    StrLen $3 $2
    StrCpy $4 $0 $3
    ${If} $4 == $2
      StrCpy $1 1
    ${EndIf}
    ${If} $1 != 1
      DetailPrint "chronogazer: サービス ChronoGazer は別の場所の実行ファイル ($0) で登録されています。インストール先 $INSTDIR と違うため、インストールを中止しました。"
      MessageBox MB_OK|MB_ICONSTOP "サービス ChronoGazer は、別の場所の実行ファイルで登録されています。$\r$\n$\r$\n登録: $0$\r$\nインストール先: $INSTDIR$\r$\n$\r$\nこのまま入れると、サービスは古い実行ファイルを使い続けます。今の場所へインストールし直すか、先に今の ChronoGazer をアンインストールしてください。$\r$\n$\r$\nまだ何も変更していません。" /SD IDOK
      Abort "サービス ChronoGazer の登録の場所がインストール先と違うため、インストールを中止しました。"
    ${EndIf}
  ${EndIf}
!macroend

; O-3: 旧い利用者単位の版があれば案内する（上の「旧い利用者単位の版」節）。
!macro WarnAboutLegacyCurrentUserInstall
  ReadRegStr $0 HKCU "${CHRONOGAZER_UNINSTKEY}" "UninstallString"
  ${If} $0 != ""
    ReadRegStr $1 HKCU "${CHRONOGAZER_UNINSTKEY}" "InstallLocation"
    DetailPrint "chronogazer: 旧い利用者単位の版の ChronoGazer が残っています（場所: $1）。このインストーラは消しません。設定の「アプリ」（インストールされているアプリ）から ChronoGazer のうち利用者単位の版をアンインストールするか、$0 を実行してください。"
    MessageBox MB_OK|MB_ICONINFORMATION "以前の版の ChronoGazer（利用者単位のインストール、場所: $1）が残っています。$\r$\n$\r$\nこのインストーラはそれを消さずに、全利用者向けの版を C:\Program Files に入れます。古い方は、インストールの後に設定の「アプリ」から ChronoGazer のうち利用者単位の版をアンインストールしてください（アンインストーラの「アプリのデータを削除する」にチェックを付けると、以前の版の DB (%APPDATA%\dev.tyaro.chronogazer) も消えます。残したいときは付けないでください）。" /SD IDOK
  ${EndIf}
!macroend

; --- フック本体 ---------------------------------------------------------

; 事前の確かめ（上の「事前の確かめ」節）。テンプレートの当て直し P3 が Section
; WebView2 の先頭に挿入する（WebView2 を入れる前に、何も変えずに止められるものを
; 全部済ませる）。
!macro ChronoGazerInstallPreflight
  !insertmacro RequireDefaultInstallDir
  !insertmacro RequireRegisteredServiceInInstallDir
  !insertmacro RequirePreviousInstallInInstallDir
  !insertmacro WarnAboutLegacyCurrentUserInstall
  !insertmacro QuitShellForUpdate "インストール" 0 "まだ何も変更していません"
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; (3) WebView2 の取得の間に起動されたシェルの再確認（無ければすぐ返る）。
  !insertmacro QuitShellForUpdate "インストール" 0 "まだ何も変更していません"
  ; (4)(5) サービスの停止と、止まったことの確かめ。
  !insertmacro StopServiceIfRunning "ChronoGazer" $ChronoGazerWasRunning
  !insertmacro RequireChronoGazerServiceInactive "インストール"
!macroend

; `.onInstFailed`（上の「.onInstFailed」節。テンプレートの当て直し P4 が挿入する）。
!macro ChronoGazerOnInstFailed
  ${If} $ChronoGazerWasRunning == "1"
    DetailPrint "chronogazer: インストールが途中で止まったため、上書き前に止めた ChronoGazer サービスを戻します..."
    !insertmacro RestartChronoGazerServiceOrWarn "インストール"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "chronogazer: Windows サービス (ChronoGazer) を登録しています..."
  ; ExecWait は起動できないとエラーフラグを立てるだけで $0 を変えない（前の値が
  ; "0" だと成功に見える）ので、フラグも見る。
  ClearErrors
  ExecWait '"$INSTDIR\chronogazer-elev.exe" service-install' $0
  ${If} ${Errors}
    StrCpy $0 "exec-error"
  ${EndIf}
  ${If} $0 != 0
    DetailPrint "chronogazer: ChronoGazer サービスの登録に失敗しました（終了コード $0）。管理者権限の PowerShell から手動で次を実行してください: $\"$INSTDIR\chronogazer-elev.exe$\" service-install （apps/chronogazer/README.md「インストーラ」「昇格ヘルパー chronogazer-elev」参照）"
    ; 失敗は成功扱いにしない（#972 のオーナーレビュー P1）。ただし上書きの前に動いて
    ; いたサービスは、ここでも再開を試みる（#972 のオーナーレビュー（fb82551）:
    ; 再開しないと、アップグレードの失敗で記録が止まったままになる）。登録は前回の
    ; ままなので、新しい exe・前回の ACL で動く。再開の結果は問いに書き、Abort で
    ; インストーラをエラーで終える（silent では終了コード 2）。ファイルは入った
    ; ままなので、手で service-install を通すか、入れ直す。
    ; StartServiceIfStoppedAndWait は $0・$1 を上書きし、フラグを "0" に戻すので、
    ; 終了コードを $2 に、再開を試みたか（フラグが "1" だったか）を $3 に控える。
    StrCpy $2 $0
    StrCpy $3 $ChronoGazerWasRunning
    !insertmacro StartServiceIfStoppedAndWait "ChronoGazer" $ChronoGazerWasRunning
    ; マクロがフラグを "0" にするが、Abort の前に明示して .onInstFailed に再度
    ; 戻させない。
    StrCpy $ChronoGazerWasRunning "0"
    ${If} $3 != "1"
      DetailPrint "chronogazer: 上書きの前にサービスは動いていなかったため、再開は試みていません。"
      MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスの登録に失敗しました（終了コード $2）。$\r$\n$\r$\nファイルは入りましたが、記録エンジンのサービスは使えません。サービスは動いていません。$\r$\n$\r$\n管理者権限の PowerShell から次を実行してください:$\r$\n$\"$INSTDIR\chronogazer-elev.exe$\" service-install" /SD IDOK
    ${ElseIf} $0 == "0"
      DetailPrint "chronogazer: 登録に失敗しましたが、上書き前に動いていたサービスは再開しました。"
      MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスの登録・ACL の確認に失敗しました（終了コード $2）。$\r$\n$\r$\n上書きの前に動いていたサービスは、前回の登録のまま、いま入ったファイルで再開しました。登録と ACL は整っていない可能性があります。$\r$\n$\r$\n管理者権限の PowerShell から次を実行してください:$\r$\n$\"$INSTDIR\chronogazer-elev.exe$\" service-install" /SD IDOK
    ${Else}
      DetailPrint "chronogazer: 登録に失敗し、上書き前に動いていたサービスも再開できませんでした（$0）。"
      MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスの登録・ACL の確認に失敗しました（終了コード $2）。$\r$\n$\r$\n上書きの前に動いていたサービスも再開できませんでした（$0）。サービスは止まったままです。サービスの状態を確かめ（sc query ChronoGazer）、管理者権限の PowerShell から次を実行したあと、sc start ChronoGazer で開始するか、シェルの「運用モード」で開始してください:$\r$\n$\"$INSTDIR\chronogazer-elev.exe$\" service-install" /SD IDOK
    ${EndIf}
    Abort "ChronoGazer サービスの登録に失敗したため、インストールを失敗として終えました。"
  ${Else}
    DetailPrint "chronogazer: サービス (ChronoGazer) の登録・Operators グループ・サービス ACL・データの ACL を確認しました。起動種別は手動（Demand）です - OS 再起動では開始しません（自動開始は chronogazer-elev.exe autostart-enable か、シェルの設定「運用モード」）。"
  ${EndIf}

  ; PREINSTALL で止めたときだけ再開し、RUNNING になるまで最大 30 秒待つ。新規
  ; インストールでは未設定のため実行されない。再開できなければ成功扱いにしない
  ; （#972 のオーナーレビュー）: ファイルと登録は入っているので、案内して Abort で
  ; インストーラをエラーで終える。
  !insertmacro StartServiceIfStoppedAndWait "ChronoGazer" $ChronoGazerWasRunning
  ${If} $0 != "0"
    DetailPrint "chronogazer: 上書き前に止めた ChronoGazer サービスを再開できませんでした（$0）。ファイルと登録は入っています。管理者権限の PowerShell から sc start ChronoGazer で開始するか、シェルの「運用モード」で開始してください。"
    MessageBox MB_OK|MB_ICONSTOP "上書き前に止めた ChronoGazer サービスを再開できませんでした（$0）。$\r$\n$\r$\nファイルとサービスの登録は入りましたが、サービスは止まったままです。サービスの状態を確かめ（sc query ChronoGazer）、管理者権限の PowerShell から sc start ChronoGazer で開始するか、シェルの「運用モード」で開始してください。" /SD IDOK
    Abort "ChronoGazer サービスを再開できなかったため、インストールを失敗として終えました。"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; ここではサービスの登録を解除しない。解除は Section Uninstall の再確認
  ; （src/main.rs の P2-uninstall-check）と使用中の exe の確かめ（P6 (a)）の後、
  ; ファイルを消す直前の UnregisterChronoGazerService で行う（再確認の
  ; [キャンセル] で、登録だけ消えてファイルが残る状態を作らないため。#972 の
  ; オーナーレビュー）。消した後の確かめは P6 (d)（上の「PREUNINSTALL」節）。
  !insertmacro QuitShellForUpdate "アンインストール" 0 "まだ何も変更していません"
  !insertmacro StopServiceIfRunning "ChronoGazer" $ChronoGazerWasRunning
  !insertmacro RequireChronoGazerServiceInactive "アンインストール"
!macroend

; サービスの登録解除（Section Uninstall の再確認と使用中の exe の確かめの直後に
; 挿入される）。
; 失敗したら止めたサービスを戻して `Abort` する（ファイルを消す前に止まるので、
; 登録されたサービスが消えた exe を指す状態にならない）。chronogazer-elev.exe も
; chronogazer-server.exe も無いときは、登録が残っていれば `sc delete` で解除し、それも
; 失敗したら同じく止める（登録が無ければそのまま続ける。#972 のオーナーレビュー P2）。
!macro UnregisterChronoGazerService
  DetailPrint "chronogazer: Windows サービス (ChronoGazer) の登録を解除しています..."
  ; 登録が無ければ解除は要らない（インストールの登録が失敗した・利用者が先に
  ; service-uninstall した）。elev の service-uninstall は未登録を終了コード 2 で
  ; 返すので、先に確かめないとアンインストールできなくなる（#972 の Codex の指摘）。
  ReadRegStr $1 HKLM "SYSTEM\CurrentControlSet\Services\ChronoGazer" "ImagePath"
  ; ExecWait は起動できないとエラーフラグを立てるだけで $0 を変えないので、フラグも見る。
  ClearErrors
  ${If} $1 == ""
    DetailPrint "chronogazer: サービス ChronoGazer は登録されていないので、登録の解除は飛ばします。"
    StrCpy $0 0
  ${ElseIf} ${FileExists} "$INSTDIR\chronogazer-elev.exe"
    ExecWait '"$INSTDIR\chronogazer-elev.exe" service-uninstall' $0
  ${ElseIf} ${FileExists} "$INSTDIR\chronogazer-server.exe"
    DetailPrint "chronogazer: chronogazer-elev.exe が見つからないため chronogazer-server.exe uninstall にフォールバックします"
    ExecWait '"$INSTDIR\chronogazer-server.exe" uninstall' $0
  ${Else}
    ; 両方無くても登録が残っていれば sc delete で解除する。解除できなければ
    ; 下の失敗の扱いでファイルを消す前に止める（#972 のオーナーレビュー P2）。
    DetailPrint "chronogazer: chronogazer-elev.exe も chronogazer-server.exe も見つからないため、sc delete ChronoGazer で登録を解除します..."
    nsExec::ExecToLog 'sc.exe delete ChronoGazer'
    Pop $0
  ${EndIf}
  ${If} ${Errors}
    StrCpy $0 "exec-error"
  ${EndIf}
  ${If} $0 != 0
    ; 終了コードを $2 に控える（StartServiceIfStoppedAndWait が $0・$1 を使うため）。
    StrCpy $2 $0
    DetailPrint "chronogazer: ChronoGazer の登録解除に失敗しました（終了コード $2）。apps/chronogazer/README.md「昇格ヘルパー chronogazer-elev」の手順で手動対応してください"
    ; 止めたサービスを戻す（戻せなければそれも案内される）。
    !insertmacro RestartChronoGazerServiceOrWarn "アンインストール"
    MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスの登録解除に失敗しました（終了コード $2）。ファイルは消さずにアンインストールを中止します。止めたサービスは再開を試みました。apps/chronogazer/README.md「昇格ヘルパー chronogazer-elev」の手順で対処してから、もう一度アンインストールしてください。" /SD IDOK
    Abort "ChronoGazer サービスの登録解除に失敗したため、アンインストールを中止しました。"
  ${EndIf}

  ; %ProgramData%\ChronoGazer\（DB・時系列データ・Hub のキー・ログ）は意図的に
  ; 削除しない（再インストールでデータが戻ることを優先する）。完全削除は利用者が
  ; 手動で行う。
!macroend
