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
; **どのステップが失敗してもインストーラ本体は中断させない**（DetailPrint で案内
; するだけ、Hub と同じ）。唯一の例外は「シェルの終了」の待ちで利用者が [キャンセル]
; を選んだとき（下記）で、このときはまだ何も変えていないので `Abort` で止める。
;
; ## PREINSTALL
;
;   (0) インストール先が Program Files の中かを確かめる。外なら案内して `Abort`
;       （まだ何も変えていない）。`chronogazer-elev service-install` は Program Files の
;       外の `chronogazer-server.exe` を LocalSystem のサービスに登録しない（権限の
;       昇格を防ぐため）ので、外に入れるとサービスの無いインストールが黙って
;       できてしまう。PerMachine は既定の場所を決めるだけで、ディレクトリの頁や
;       `/D=` で外を選べる（#972 の Codex の指摘）。
;   (1) O-3: 旧い利用者単位の版の案内（下記「旧い利用者単位の版」節）。
;   (2) シェルの終了（下記「シェルの終了」節）。[キャンセル] ならここで中止 -
;       サービスはまだ止めていないので、何も止めずに終わる。
;   (3) サービス `ChronoGazer` が Running なら停止し、$ChronoGazerWasRunning に
;       "1" を記録（共通部品の StopServiceIfRunning。未登録・停止中は静かに素通り）。
;
; ## POSTINSTALL
;
;   (1) `chronogazer-elev.exe service-install`（登録 → `ChronoGazer Operators` →
;       サービスの ACL → データと secrets の ACL。README「昇格ヘルパー」の表）。
;       冪等 - 登録済みなら登録は変えない。データのルートが使用中（サービスが
;       まだ止まっていない・コンソール版が動いている）なら、ACL の付け直しだけを
;       飛ばして警告 1 行で 0（#971、Hub と同じ。警告は標準エラー出力だけで
;       インストーラには残らない）。`--install-user-sid` は渡さない - インストール
;       利用者は UAC を通した利用者になる（README「インストーラ」の既知の制限、O-2）。
;   (2) PREINSTALL で止めたときだけ `sc start`（StartServiceIfStopped）。新規
;       インストールでは $ChronoGazerWasRunning が未設定なので何も起動しない
;       （収集を勝手に始めない）。
;   ファイアウォールの規則は作らない（LAN 公開は利用者が有効にしたときに
;   シェル／サービスが行う）。
;
; ## PREUNINSTALL
;
;   シェルの終了（PREINSTALL と同じ待ち。[キャンセル] でアンインストールを中止）
;   → サービスの停止、まで。**登録の解除はここでしない**。Section Uninstall の
;   再確認（下記）の後に UnregisterChronoGazerService が `chronogazer-elev.exe
;   service-uninstall`（無ければ `chronogazer-server.exe uninstall`、それも無ければ
;   手動の案内）を行う。非 0 で失敗したら止めたサービスを再開し、メッセージを出して
;   `Abort` する（ファイルを消す前に止まる）。
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
; つまり Section Install では「フックの待ち → サービスの停止 → 再確認 → ファイルの
; 上書き」の順になり、再確認の [キャンセル] は止めたサービスを戻してから `Abort`
; する（`restart_service_on_cancel` = 1）。Section Uninstall も同じ順
; （PREUNINSTALL = シェルの終了とサービスの停止 → 再確認 → サービスの登録解除 →
; ファイルの削除）で、再確認の [キャンセル] は止めたサービスを戻して `Abort`
; する（まだ何も変更していない）。
; 残る隙間は「再確認の後・ファイルの上書きの前にシェルが起動される」の一瞬だけで、
; そのときは NSIS 自身の「ファイルが使用中」の中止／再試行／無視の窓になる
; （強制終了にはならない）。
;
; 当て直したテンプレートも `QuitShellForUpdate` を `!insertmacro` するので、
; マクロの名前・引数の並びを変えるときは src/main.rs の TEMPLATE_PATCHES も
; 一緒に直す（試験 `patch_template_applies_p1_and_p2` が引数の並びを見る）。
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
; service-common.nsh（StopServiceIfRunning・StartServiceIfStopped）。製品固有の
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
!define CHRONOGAZER_LEGACY_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ChronoGazer"

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
;     PREINSTALL で止めたサービスを StartServiceIfStopped で戻す（Section Install
;     の再確認用。フックの中では止める前なので 0）。
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
      !insertmacro StartServiceIfStopped "ChronoGazer" $ChronoGazerWasRunning
    !endif
    Abort "ChronoGazer のデスクトップシェルが終了していないため、${action}を中止しました。"

  shellquit_done_${ShellQuitID}:
  !undef ShellQuitID
!macroend

; PREINSTALL (0): $INSTDIR が Program Files（64 ビット・x86）の中でなければ止める。
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

; O-3: 旧い利用者単位の版があれば案内する（上の「旧い利用者単位の版」節）。
!macro WarnAboutLegacyCurrentUserInstall
  ReadRegStr $0 HKCU "${CHRONOGAZER_LEGACY_UNINSTKEY}" "UninstallString"
  ${If} $0 != ""
    ReadRegStr $1 HKCU "${CHRONOGAZER_LEGACY_UNINSTKEY}" "InstallLocation"
    DetailPrint "chronogazer: 旧い利用者単位の版の ChronoGazer が残っています（場所: $1）。このインストーラは消しません。設定の「アプリ」（インストールされているアプリ）から ChronoGazer のうち利用者単位の版をアンインストールするか、$0 を実行してください。"
    MessageBox MB_OK|MB_ICONINFORMATION "以前の版の ChronoGazer（利用者単位のインストール、場所: $1）が残っています。$\r$\n$\r$\nこのインストーラはそれを消さずに、全利用者向けの版を C:\Program Files に入れます。古い方は、インストールの後に設定の「アプリ」から ChronoGazer のうち利用者単位の版をアンインストールしてください（アンインストーラの「アプリのデータを削除する」にチェックを付けると、以前の版の DB (%APPDATA%\dev.tyaro.chronogazer) も消えます。残したいときは付けないでください）。" /SD IDOK
  ${EndIf}
!macroend

; --- フック本体 ---------------------------------------------------------

!macro NSIS_HOOK_PREINSTALL
  !insertmacro RequireInstallDirInProgramFiles
  !insertmacro WarnAboutLegacyCurrentUserInstall
  !insertmacro QuitShellForUpdate "インストール" 0 "まだ何も変更していません"
  !insertmacro StopServiceIfRunning "ChronoGazer" $ChronoGazerWasRunning
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "chronogazer: Windows サービス (ChronoGazer) を登録しています..."
  ExecWait '"$INSTDIR\chronogazer-elev.exe" service-install' $0
  ${If} $0 != 0
    DetailPrint "chronogazer: ChronoGazer サービスの登録に失敗しました（終了コード $0）。管理者権限の PowerShell から手動で次を実行してください: $\"$INSTDIR\chronogazer-elev.exe$\" service-install （apps/chronogazer/README.md「インストーラ」「昇格ヘルパー chronogazer-elev」参照）"
  ${Else}
    DetailPrint "chronogazer: サービス (ChronoGazer) の登録・Operators グループ・サービス ACL・データの ACL を確認しました。起動種別は手動（Demand）です - OS 再起動では開始しません（自動開始は chronogazer-elev.exe autostart-enable か、シェルの設定「運用モード」）。"
  ${EndIf}

  ; PREINSTALL で止めたときだけ再開する。新規インストールでは未設定のため実行されない。
  !insertmacro StartServiceIfStopped "ChronoGazer" $ChronoGazerWasRunning
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; ここではサービスの登録を解除しない。解除は Section Uninstall の再確認
  ; （src/main.rs の P2-uninstall-check）の後、ファイルを消す直前の
  ; UnregisterChronoGazerService で行う（再確認の [キャンセル] で、登録だけ消えて
  ; ファイルが残る状態を作らないため。#972 のオーナーレビュー）。
  !insertmacro QuitShellForUpdate "アンインストール" 0 "まだ何も変更していません"
  !insertmacro StopServiceIfRunning "ChronoGazer" $ChronoGazerWasRunning
!macroend

; サービスの登録解除（Section Uninstall の再確認の直後に挿入される）。
; 失敗したら止めたサービスを戻して `Abort` する（ファイルを消す前に止まるので、
; 登録されたサービスが消えた exe を指す状態にならない）。chronogazer-elev.exe も
; chronogazer-server.exe も無いときは守るものが無いので手動の案内だけで続ける。
!macro UnregisterChronoGazerService
  DetailPrint "chronogazer: Windows サービス (ChronoGazer) の登録を解除しています..."
  ${If} ${FileExists} "$INSTDIR\chronogazer-elev.exe"
    ExecWait '"$INSTDIR\chronogazer-elev.exe" service-uninstall' $0
  ${ElseIf} ${FileExists} "$INSTDIR\chronogazer-server.exe"
    DetailPrint "chronogazer: chronogazer-elev.exe が見つからないため chronogazer-server.exe uninstall にフォールバックします"
    ExecWait '"$INSTDIR\chronogazer-server.exe" uninstall' $0
  ${Else}
    DetailPrint "chronogazer: chronogazer-elev.exe も chronogazer-server.exe も見つからないため、サービス登録解除を手動で行ってください（管理者権限の PowerShell から sc delete ChronoGazer）"
    StrCpy $0 0
  ${EndIf}
  ${If} $0 != 0
    ; 終了コードを $1 に控える（StartServiceIfStopped が $0 を使うため）。
    StrCpy $1 $0
    DetailPrint "chronogazer: ChronoGazer の登録解除に失敗しました（終了コード $1）。apps/chronogazer/README.md「昇格ヘルパー chronogazer-elev」の手順で手動対応してください"
    !insertmacro StartServiceIfStopped "ChronoGazer" $ChronoGazerWasRunning
    MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスの登録解除に失敗しました（終了コード $1）。ファイルは消さずにアンインストールを中止します。止めたサービスは再開しました。apps/chronogazer/README.md「昇格ヘルパー chronogazer-elev」の手順で対処してから、もう一度アンインストールしてください。" /SD IDOK
    Abort "ChronoGazer サービスの登録解除に失敗したため、アンインストールを中止しました。"
  ${EndIf}

  ; %ProgramData%\ChronoGazer\（DB・時系列データ・Hub のキー・ログ）は意図的に
  ; 削除しない（再インストールでデータが戻ることを優先する）。完全削除は利用者が
  ; 手動で行う。
!macroend
