; banto-hub 一体インストーラの NSIS フック（T5-2 → I1、
; docs/banto-hub-installer-design.md §4.3・§7 決定4/6・
; docs/banto-hub-operations.md §12）。
;
; tauri-bundler の NSIS バンドラ（NsisSettings::installer_hooks、
; apps/banto-hub/installer/src/main.rs 参照）が対応する4フックのうち、
; このファイルは NSIS_HOOK_PREINSTALL・NSIS_HOOK_POSTINSTALL・
; NSIS_HOOK_PREUNINSTALL の3つを実装する（NSIS_HOOK_POSTUNINSTALL は
; 使わない - アンインストール後に追加でやることが無い。
; %ProgramData%\BantoHub\ と profile は意図的に残す、下記「アンインス
; トール」節参照）。
;
; `$INSTDIR` は、tauri-bundler 2.9.4 の installer.nsi テンプレート側で
; Section Install / Section Uninstall 内の該当箇所まで展開済みの変数と
; してこの位置で参照できる（ソース確認済み: crates/tauri-bundler/src/
; bundle/windows/nsis/installer.nsi のタグ tauri-bundler-v2.9.4）。I1 で
; main バイナリがシェル（`banto-hub-shell`）になったため、以前使っていた
; `${MAINBINARYNAME}` は「シェルの exe 名」を指すようになった - Hub 本体・
; elev・サイドカーは main ではないので、このフックでは exe ファイル名を
; 直接リテラルで書く（`banto-hub.exe`/`banto-hub-elev.exe`/
; `banto-hub-sink.exe`、いずれも main.rs の `BundleBinary::new` と一致させる
; こと）。
;
; このインストーラ自体を「PerMachine」モードでビルドしている
; （apps/banto-hub/installer/src/main.rs の NsisSettings::install_mode）
; ため、UAC 昇格済みで実行されており、各フックも常に管理者権限で動く。
;
; **`%ProgramData%` の参照方法**: NSIS に `$COMMONAPPDATA` という定数は
; 存在しない（誤って一度使い、`makensis` の
; `unknown variable/constant "COMMONAPPDATA\..." detected, ignoring`
; 警告で実機ビルド時に発覚した - ローカルビルドでの検証で見つけた実例と
; して記録しておく）。正しい取り方は、tauri-bundler の `SetContext` マクロ
; （nsis/utils.nsh、`.onInit`/`un.onInit` から PerMachine では
; `SetShellVarContext all` として既に呼ばれ済み）により、`$APPDATA` が
; ユーザー毎ではなく **全ユーザー共通の `%ProgramData%`** を指すようになる
; という NSIS の標準的な挙動を使うこと - このフックは `$APPDATA` を
; `%ProgramData%` の意味で使う（後述、常に PerMachine 前提のこのインス
; トーラでは安全）。
;
; **どのステップが失敗してもインストーラ本体は中断させない** - DetailPrint
; で案内するだけに留め、必要なら docs/banto-hub-operations.md §12 の手順で
; 手動対応してもらう（T5-2 以来の一貫した方針）。
;
; ## PREINSTALL（§4.3 表・§7 決定4「自動で停止し、動いていたものだけ再開」）
;
; 上書きインストールでは、稼働中の exe をファイルコピーで差し替えられない
; （File 命令が失敗する/ロックされたファイルを残したまま新旧混在になる）
; ため、コピー前に次を止める:
;   (1) BantoHubSink が Running なら停止し、$SinkWasRunning に "1" を記録
;   (2) BantoHub が Running なら停止し、$BantoHubWasRunning に "1" を記録
;   (3) banto-hub-shell.exe が動いていれば taskkill で終了する
; 新規インストール（サービス未登録、`sc query` が失敗する = 1060 相当）は、
; RUNNING の文字列がそもそも見つからないため何も起きず静かに素通りする -
; 「見つからない」と「止まっている」を区別する必要はない（どちらも「今回は
; 何もしない」で同じ扱いでよい）。
;
; ## POSTINSTALL（§4.3 表・§7 決定5/6）
;
; (1) `banto-hub-elev.exe service-install`（Hub のサービス登録 →
;     `BantoHub Operators` 作成 → サービス ACL → profile ACL を一括、
;     apps/banto-hub/core/src/service_elevated.rs
;     `service_install_and_delegate` 参照）- 旧 `banto-hub.exe install`
;     を置き換える。
; (2) `banto-hub-sink.exe install`（サービス登録のみ、冪等）
; (3) `banto-hub-sink.exe grant-service-acl`（同梱の elev 経由で ACE 付与、
;     apps/banto-hub-sink/src/service.rs 参照）
; (4) `%ProgramData%\BantoHub\`（PerMachine では `$APPDATA` が指す先、
;     上記「`%ProgramData%` の参照方法」節参照）を作成し、
;     `banto-hub-sink.toml.example` が無ければ配置する。**実ファイルの
;     `banto-hub-sink.toml` は作らない** - API キーは Hub を起動してから
;     発行するものなので、インストーラには書けない（§4.4）。複製先の
;     `.example` 自体の存在で冪等性を判定する（既にあれば上書きしない -
;     手で調整済みの雛形を誤って戻さないため）。
; (5) PREINSTALL で止めたものだけ `sc start` で再開する（Hub → Sink の順、
;     §4.3「起動していたものだけ再開」）。**新規インストールでは
;     $BantoHubWasRunning/$SinkWasRunning が未設定のままなので何も
;     起動しない**（T17-4 の「収集を勝手に始めない」方針を維持）。
;
; いずれのサブコマンドも冪等 - 既に登録済みのサービスに対して `install`/
; `service-install` を呼んでも既存設定は変更しない（T17-4、
; apps/banto-hub/core/src/service_install.rs 参照）。
;
; ## PREUNINSTALL（§4.3 表・§7 決定7）
;
; シェルを終了し、サイドカー→Hub の順でサービス登録を解除する。
; `banto-hub-elev.exe` が見つからない壊れたインストールに備えて
; `banto-hub.exe uninstall`（同等の処理、apps/banto-hub/core/src/bin/
; banto_hub/win_service.rs 参照）へのフォールバックを持つ。
; `%ProgramData%\BantoHub\`（DB・サイドカー設定）とサービスログは
; **削除しない**（§4.5「アンインストール」・オーナー決定 §7-7 - 再
; インストールで設定・データが戻ることを優先する）。

!include "LogicLib.nsh"

Var BantoHubWasRunning
Var SinkWasRunning
Var StopWaitCounter

; --- 共通ヘルパー -----------------------------------------------------

; `service_name` の Windows サービスが Running なら `sc stop` し、
; `STOPPED` になるまで最大30秒ポーリングする。呼び出し前の状態
; （止めた=1／もともと止まっていた・未登録=0）を `remember_var`
; （`$BantoHubWasRunning`/`$SinkWasRunning` を渡す）に記録する。
;
; `sc query` の生出力を NSIS の文字列関数でパースする代わりに
; `cmd /c "sc query ... | findstr /C:"RUNNING" >nul"` の終了コード
; （0=見つかった、1=見つからない）だけを見る - サービス未登録
; （`sc query` 自体が 1060 相当で失敗する場合を含む）でも findstr が
; "RUNNING" を見つけられず終了コード1になるだけなので、「未登録」と
; 「登録済みだが停止中」を区別する必要が無く、この呼び出し1つで両方
; 静かに扱える（モジュール doc「PREINSTALL」節参照）。
;
; 同じマクロを BantoHubSink・BantoHub の2回呼ぶため、ラベルの衝突を
; 避ける必要がある - tauri-bundler 自身の `CheckIfAppIsRunning`
; マクロ（nsis/utils.nsh）と同じ `${__LINE__}` を使った一意化を踏襲する。
!macro StopServiceIfRunning service_name remember_var
  !define UniqueID ${__LINE__}

  StrCpy ${remember_var} "0"
  nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"RUNNING" >nul'
  Pop $0
  ${If} $0 == "0"
    StrCpy ${remember_var} "1"
    DetailPrint "banto-hub: Windows サービス (${service_name}) を停止しています..."
    nsExec::ExecToStack 'sc stop "${service_name}"'
    Pop $0

    StrCpy $StopWaitCounter 0
    stopsvc_wait_${UniqueID}:
      nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"STOPPED" >nul'
      Pop $0
      ${If} $0 == "0"
        Goto stopsvc_done_${UniqueID}
      ${EndIf}
      IntOp $StopWaitCounter $StopWaitCounter + 1
      ${If} $StopWaitCounter >= 30
        DetailPrint "banto-hub: (${service_name}) の停止待ちが30秒でタイムアウトしました。手動で状態を確認してください（docs/banto-hub-operations.md §12）"
        Goto stopsvc_done_${UniqueID}
      ${EndIf}
      Sleep 1000
      Goto stopsvc_wait_${UniqueID}
    stopsvc_done_${UniqueID}:
  ${EndIf}

  !undef UniqueID
!macroend

; `banto-hub-shell.exe` が動いていれば `taskkill` で終了する
; （single-instance のシェルは差し替え中のファイルロックを持ちうるため）。
; PREINSTALL・PREUNINSTALL の両方から呼ぶ。
!macro TerminateShellIfRunning
  nsExec::ExecToStack 'cmd /c tasklist /FI "IMAGENAME eq banto-hub-shell.exe" | findstr /I "banto-hub-shell.exe" >nul'
  Pop $0
  ${If} $0 == "0"
    DetailPrint "banto-hub: デスクトップシェル (banto-hub-shell.exe) を終了しています..."
    nsExec::ExecToStack 'taskkill /IM banto-hub-shell.exe'
    Pop $0
    Sleep 500
  ${EndIf}
!macroend

; --- フック本体 ---------------------------------------------------------

!macro NSIS_HOOK_PREINSTALL
  !insertmacro StopServiceIfRunning "BantoHubSink" $SinkWasRunning
  !insertmacro StopServiceIfRunning "BantoHub" $BantoHubWasRunning
  !insertmacro TerminateShellIfRunning
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "banto-hub: Windows サービス (BantoHub) を登録しています..."
  ExecWait '"$INSTDIR\banto-hub-elev.exe" service-install' $0
  ${If} $0 != 0
    DetailPrint "banto-hub: BantoHub サービス登録に失敗しました（終了コード $0）。管理者権限の PowerShell から手動で次を実行してください: $\"$INSTDIR\banto-hub-elev.exe$\" service-install （docs/banto-hub-operations.md §12 参照）"
  ${Else}
    DetailPrint "banto-hub: サービス (BantoHub) の登録・Operators グループ・サービス ACL・profile ACL を確認しました。起動種別は手動（Demand）です - OS 再起動では開始しません。"
  ${EndIf}

  DetailPrint "banto-hub: DB Sink サイドカーサービス (BantoHubSink) を登録しています..."
  ExecWait '"$INSTDIR\banto-hub-sink.exe" install' $0
  ${If} $0 != 0
    DetailPrint "banto-hub: BantoHubSink サービス登録に失敗しました（終了コード $0）。手動で次を実行してください: $\"$INSTDIR\banto-hub-sink.exe$\" install"
  ${EndIf}

  DetailPrint "banto-hub: BantoHubSink へ Operators のサービス ACL を付与しています..."
  ExecWait '"$INSTDIR\banto-hub-sink.exe" grant-service-acl' $0
  ${If} $0 != 0
    DetailPrint "banto-hub: grant-service-acl に失敗しました（終了コード $0）。手動で次を実行してください: $\"$INSTDIR\banto-hub-sink.exe$\" grant-service-acl"
  ${EndIf}

  ; §4.4: ProgramData（PerMachine では $APPDATA が指す - モジュール doc
  ; 「%ProgramData% の参照方法」節参照）を作成し、サイドカー設定の雛形
  ; （.example のみ）を配置する。既にあれば上書きしない
  ; （モジュール doc「POSTINSTALL」節参照）。
  CreateDirectory "$APPDATA\BantoHub"
  ${IfNot} ${FileExists} "$APPDATA\BantoHub\banto-hub-sink.toml.example"
    CopyFiles /SILENT "$INSTDIR\banto-hub-sink.toml.example" "$APPDATA\BantoHub\banto-hub-sink.toml.example"
    DetailPrint "banto-hub: サイドカー設定の雛形を配置しました: $APPDATA\BantoHub\banto-hub-sink.toml.example （api_key を発行後、banto-hub-sink.toml にリネームしてください）"
  ${EndIf}

  ; §7 決定4: PREINSTALL で止めたものだけ再開する（Hub → Sink の順）。
  ; 新規インストールでは両変数とも未設定のため、このブロックは実行されない。
  ${If} $BantoHubWasRunning == "1"
    DetailPrint "banto-hub: 上書き前に稼働していた BantoHub サービスを再開します..."
    nsExec::ExecToStack 'sc start "BantoHub"'
    Pop $0
  ${EndIf}
  ${If} $SinkWasRunning == "1"
    DetailPrint "banto-hub: 上書き前に稼働していた BantoHubSink サービスを再開します..."
    nsExec::ExecToStack 'sc start "BantoHubSink"'
    Pop $0
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro TerminateShellIfRunning

  DetailPrint "banto-hub: DB Sink サイドカーサービス (BantoHubSink) の登録を解除しています..."
  ExecWait '"$INSTDIR\banto-hub-sink.exe" uninstall' $0
  ${If} $0 != 0
    DetailPrint "banto-hub: BantoHubSink の登録解除に失敗しました（終了コード $0）。手動で次を実行してください: $\"$INSTDIR\banto-hub-sink.exe$\" uninstall"
  ${EndIf}

  DetailPrint "banto-hub: Windows サービス (BantoHub) の登録を解除しています..."
  ${If} ${FileExists} "$INSTDIR\banto-hub-elev.exe"
    ExecWait '"$INSTDIR\banto-hub-elev.exe" service-uninstall' $0
  ${ElseIf} ${FileExists} "$INSTDIR\banto-hub.exe"
    DetailPrint "banto-hub: banto-hub-elev.exe が見つからないため banto-hub.exe uninstall にフォールバックします"
    ExecWait '"$INSTDIR\banto-hub.exe" uninstall' $0
  ${Else}
    DetailPrint "banto-hub: banto-hub-elev.exe も banto-hub.exe も見つからないため、サービス登録解除を手動で行ってください（管理者権限の PowerShell から sc delete BantoHub）"
    StrCpy $0 0
  ${EndIf}
  ${If} $0 != 0
    DetailPrint "banto-hub: BantoHub の登録解除に失敗しました（終了コード $0）。docs/banto-hub-operations.md §12 の手順で手動対応してください"
  ${EndIf}

  ; %ProgramData%\BantoHub\（DB・サイドカー設定・サービスログ）は
  ; 意図的に削除しない（§4.5・§7 決定7 - 再インストールで設定・データが
  ; 戻ることを優先する）。完全削除は docs の手順に従い利用者が手動で行う。
!macroend
