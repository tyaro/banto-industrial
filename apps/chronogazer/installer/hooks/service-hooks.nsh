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
;   (0) インストール先が Program Files の中かを確かめる。外なら案内して `Abort`。
;       `chronogazer-elev service-install` は Program Files の外の
;       `chronogazer-server.exe` を LocalSystem のサービスに登録しない（権限の
;       昇格を防ぐため）ので、外に入れるとサービスの無いインストールが黙って
;       できてしまう。PerMachine は既定の場所を決めるだけで、ディレクトリの頁や
;       `/D=` で外を選べる（#972 の Codex の指摘）。
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
;       「サービスは止めたまま・ファイルは一部だけ新しい」になる。停止はもう
;       頼んであるので戻す操作は無く、$ChronoGazerWasRunning を "0" にして
;       （`.onInstFailed` が `sc start` を試みないように）止める。
;   この後、テンプレートの再確認（P2、同じ QuitShellForUpdate。[キャンセル] は
;   止めたサービスを戻してから `Abort`）→ `File` でファイルの上書き。
;
; ## POSTINSTALL
;
;   (1) `chronogazer-elev.exe service-install`（登録 → `ChronoGazer Operators` →
;       サービスの ACL → データと secrets の ACL。README「昇格ヘルパー」の表）。
;       失敗（非 0、または起動できない）は成功扱いにしない: 止めたサービスは再開
;       せず、問いを出して `Abort` でインストーラをエラーで終える（#972 の
;       オーナーレビュー P1）。冪等 - 登録済みなら登録は変えない。データのルートが
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
;   対策のロック）のときだけ。silent でも呼ばれる。
;
; ## PREUNINSTALL
;
;   シェルの終了（PREINSTALL と同じ待ち。[キャンセル] でアンインストールを中止）
;   → サービスの停止 → RUNNING／*_PENDING のままなら `Abort`（(5) と同じ）、まで。
;   **登録の解除はここでしない**。Section Uninstall の再確認（下記）の後に
;   UnregisterChronoGazerService が `chronogazer-elev.exe service-uninstall`
;   （無ければ `chronogazer-server.exe uninstall`、それも無ければ登録が残って
;   いれば `sc delete ChronoGazer`）を行う。非 0（または起動できない）で失敗したら
;   止めたサービスを再開し（戻せなければそれも案内）、メッセージを出して `Abort`
;   する（ファイルを消す前に止まる）。
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
; そのときは NSIS 自身の「ファイルが使用中」の中止／再試行／無視の窓になる
; （強制終了にはならない）。
;
; 当て直したテンプレートは `QuitShellForUpdate`・`ChronoGazerInstallPreflight`・
; `UnregisterChronoGazerService`・`ChronoGazerOnInstFailed` を `!insertmacro` する
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
; tauri-bundler の Uninstall キー（`${UNINSTKEY}` と同じ値）。この版は HKLM、旧い
; 利用者単位の版は HKCU に同じ名前で書く。
!define CHRONOGAZER_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ChronoGazer"

Var ChronoGazerWasRunning
Var ShellQuitWaitCounter

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
;   `action`: 文言に出す「インストール」／「アンインストール」。
!macro RequireChronoGazerServiceInactive action
  nsExec::ExecToStack 'cmd /c sc query "ChronoGazer" | findstr /C:"RUNNING" >nul'
  Pop $0
  ${If} $0 != "0"
    nsExec::ExecToStack 'cmd /c sc query "ChronoGazer" | findstr /C:"PENDING" >nul'
    Pop $0
  ${EndIf}
  ${If} $0 == "0"
    ; 停止はもう頼んである（または頼めない状態）。戻す操作は無いので、
    ; .onInstFailed が sc start を試みないよう "0" にしてから止める。
    StrCpy $ChronoGazerWasRunning "0"
    DetailPrint "chronogazer: ChronoGazer サービスが止まっていません（RUNNING か PENDING のまま）。実行ファイルを入れ替えられないため、${action}を中止しました。"
    MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスがまだ止まっていません（停止を頼んで 30 秒待ちましたが、RUNNING か PENDING のままです）。$\r$\n$\r$\n実行ファイルを入れ替えられないため、${action}を中止しました。ファイルはまだ変更していません。サービスの状態を確かめ（sc query ChronoGazer）、止まってからもう一度実行してください。" /SD IDOK
    Abort "ChronoGazer サービスが止まっていないため、${action}を中止しました。"
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

; 事前の確かめ (0): $INSTDIR が Program Files（64 ビット・x86）の中でなければ止める。
; 判定は elev（`is_inside_program_files`）と同じく「どちらかの下」。NSIS の `==` は
; 大文字小文字を区別しない。本当のパス（ジャンクションなど）は elev が確かめ直す。
!macro RequireInstallDirInProgramFiles
  StrCpy $1 0
  StrLen $4 "$INSTDIR"
  StrCpy $0 "$PROGRAMFILES64\"
  StrLen $2 $0
  StrCpy $3 "$INSTDIR" $2
  ${If} $3 == $0
  ${AndIf} $4 > $2
    StrCpy $1 1
  ${EndIf}
  StrCpy $0 "$PROGRAMFILES32\"
  StrLen $2 $0
  StrCpy $3 "$INSTDIR" $2
  ${If} $3 == $0
  ${AndIf} $4 > $2
    StrCpy $1 1
  ${EndIf}
  ${If} $1 != 1
    DetailPrint "chronogazer: インストール先 $INSTDIR は Program Files の外です。記録エンジンのサービスを登録できないため、インストールを中止しました。"
    MessageBox MB_OK|MB_ICONSTOP "インストール先 ($INSTDIR) が Program Files の外です。$\r$\n$\r$\nChronoGazer の記録エンジンは、一般の利用者が書き換えられない Program Files の中に置かないとサービスとして登録できません。既定の場所 ($PROGRAMFILES64\ChronoGazer) のままインストールし直してください。$\r$\n$\r$\nまだ何も変更していません。" /SD IDOK
    Abort "インストール先が Program Files の外のため、インストールを中止しました。"
  ${EndIf}
!macroend

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
  !insertmacro RequireInstallDirInProgramFiles
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
    ; 失敗は成功扱いにしない（#972 のオーナーレビュー P1）。止めたサービスは再開せず
    ; （登録・ACL が整っていない状態で動かさない。.onInstFailed にも戻させない）、
    ; 問いを出してから Abort でインストーラをエラーで終える（silent では終了コード
    ; 2）。ファイルは入ったままなので、手で service-install を通すか、入れ直す。
    StrCpy $ChronoGazerWasRunning "0"
    MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスの登録に失敗しました（終了コード $0）。$\r$\n$\r$\nファイルは入りましたが、記録エンジンのサービスは使えません。上書きの前に止めたサービスは再開していません。$\r$\n$\r$\n管理者権限の PowerShell から次を実行してください:$\r$\n$\"$INSTDIR\chronogazer-elev.exe$\" service-install" /SD IDOK
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
  ; （src/main.rs の P2-uninstall-check）の後、ファイルを消す直前の
  ; UnregisterChronoGazerService で行う（再確認の [キャンセル] で、登録だけ消えて
  ; ファイルが残る状態を作らないため。#972 のオーナーレビュー）。
  !insertmacro QuitShellForUpdate "アンインストール" 0 "まだ何も変更していません"
  !insertmacro StopServiceIfRunning "ChronoGazer" $ChronoGazerWasRunning
  !insertmacro RequireChronoGazerServiceInactive "アンインストール"
!macroend

; サービスの登録解除（Section Uninstall の再確認の直後に挿入される）。
; 失敗したら止めたサービスを戻して `Abort` する（ファイルを消す前に止まるので、
; 登録されたサービスが消えた exe を指す状態にならない）。chronogazer-elev.exe も
; chronogazer-server.exe も無いときは、登録が残っていれば `sc delete` で解除し、それも
; 失敗したら同じく止める（登録が無ければそのまま続ける。#972 のオーナーレビュー P2）。
!macro UnregisterChronoGazerService
  DetailPrint "chronogazer: Windows サービス (ChronoGazer) の登録を解除しています..."
  ; ExecWait は起動できないとエラーフラグを立てるだけで $0 を変えないので、フラグも見る。
  ClearErrors
  ${If} ${FileExists} "$INSTDIR\chronogazer-elev.exe"
    ExecWait '"$INSTDIR\chronogazer-elev.exe" service-uninstall' $0
  ${ElseIf} ${FileExists} "$INSTDIR\chronogazer-server.exe"
    DetailPrint "chronogazer: chronogazer-elev.exe が見つからないため chronogazer-server.exe uninstall にフォールバックします"
    ExecWait '"$INSTDIR\chronogazer-server.exe" uninstall' $0
  ${Else}
    ; 両方無くても、登録が残っていれば sc delete で解除する。解除できなければ
    ; 下の失敗の扱いでファイルを消す前に止める（#972 のオーナーレビュー P2）。
    ReadRegStr $1 HKLM "SYSTEM\CurrentControlSet\Services\ChronoGazer" "ImagePath"
    ${If} $1 == ""
      DetailPrint "chronogazer: chronogazer-elev.exe も chronogazer-server.exe も見つかりませんが、サービス ChronoGazer は登録されていません。"
      StrCpy $0 0
    ${Else}
      DetailPrint "chronogazer: chronogazer-elev.exe も chronogazer-server.exe も見つからないため、sc delete ChronoGazer で登録を解除します..."
      nsExec::ExecToLog 'sc.exe delete ChronoGazer'
      Pop $0
    ${EndIf}
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
