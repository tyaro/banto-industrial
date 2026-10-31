; Hub（apps/banto-hub/installer/hooks/service-hooks.nsh）と ChronoGazer の
; 両インストーラの NSIS フックが共通で使う部品（#392 B3-4）。
;
; 上書きインストールで稼働中の exe を差し替えられるよう、コピー前に
; Windows サービスとデスクトップシェルを止め、止めたサービスだけを
; POSTINSTALL で `sc start` し直す、という H11 の同じ手順を 1 か所に
; 置く（docs/chronogazer-service-design.md §1.2 の表）。製品ごとに違う
; のは、サービス名・exe 名・DetailPrint の接頭辞・運用文書の参照だけ。
;
; 使い方: include する側のフックが、このファイルを `!include` する
; **前に**次を `!define` しておく（Hub の生成結果の文言を 1 文字も変え
; ないよう、製品固有の文言は引数ではなく定数で受ける）。
;   SERVICE_COMMON_LOG_PREFIX  DetailPrint の接頭辞（例: "banto-hub"）
;   SERVICE_COMMON_OPS_DOC     タイムアウト時に案内する運用文書の参照
;                              （例: "docs/banto-hub-operations.md §12"）
; どのステップが失敗してもインストーラ本体は中断させず、DetailPrint で
; 案内するだけに留める（T5-2 以来の方針）。
;
; マクロのラベルは tauri-bundler 自身の `CheckIfAppIsRunning`
; （nsis/utils.nsh）と同じ `${__LINE__}` で一意化している。同じマクロを
; 複数回呼んでも、include 越しでも衝突しない。

!ifndef SERVICE_COMMON_NSH
!define SERVICE_COMMON_NSH

!ifndef SERVICE_COMMON_LOG_PREFIX
  !error "service-common.nsh: include の前に SERVICE_COMMON_LOG_PREFIX を !define してください"
!endif
!ifndef SERVICE_COMMON_OPS_DOC
  !error "service-common.nsh: include の前に SERVICE_COMMON_OPS_DOC を !define してください"
!endif

!include "LogicLib.nsh"

Var StopWaitCounter

; `service_name` の Windows サービスが Running なら `sc stop` し、
; `STOPPED` になるまで最大30秒ポーリングする。呼び出し前の状態
; （止めた=1／もともと止まっていた・未登録=0）を `remember_var`
; （`$BantoHubWasRunning`/`$SinkWasRunning` のような変数を渡す）に記録する。
;
; `sc query` の生出力を NSIS の文字列関数でパースする代わりに
; `cmd /c "sc query ... | findstr /C:"RUNNING" >nul"` の終了コード
; （0=見つかった、1=見つからない）だけを見る - サービス未登録
; （`sc query` 自体が 1060 相当で失敗する場合を含む）でも findstr が
; "RUNNING" を見つけられず終了コード1になるだけなので、「未登録」と
; 「登録済みだが停止中」を区別する必要が無く、この呼び出し1つで両方
; 静かに扱える。
!macro StopServiceIfRunning service_name remember_var
  !define UniqueID ${__LINE__}

  StrCpy ${remember_var} "0"
  nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"RUNNING" >nul'
  Pop $0
  ${If} $0 == "0"
    StrCpy ${remember_var} "1"
    DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: Windows サービス (${service_name}) を停止しています..."
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
        DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: (${service_name}) の停止待ちが30秒でタイムアウトしました。手動で状態を確認してください（${SERVICE_COMMON_OPS_DOC}）"
        Goto stopsvc_done_${UniqueID}
      ${EndIf}
      Sleep 1000
      Goto stopsvc_wait_${UniqueID}
    stopsvc_done_${UniqueID}:
  ${EndIf}

  !undef UniqueID
!macroend

; `exe_name` のプロセスが動いていれば `taskkill` で終了する
; （single-instance のシェルは差し替え中のファイルロックを持ちうるため）。
; PREINSTALL・PREUNINSTALL の両方から呼ぶ。
!macro TerminateProcessIfRunning exe_name
  nsExec::ExecToStack 'cmd /c tasklist /FI "IMAGENAME eq ${exe_name}" | findstr /I "${exe_name}" >nul'
  Pop $0
  ${If} $0 == "0"
    DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: デスクトップシェル (${exe_name}) を終了しています..."
    nsExec::ExecToStack 'taskkill /IM ${exe_name}'
    Pop $0
    Sleep 500
  ${EndIf}
!macroend

; PREINSTALL の `StopServiceIfRunning` で止めた（`flag_var` が "1"）
; サービスだけを `sc start` で再開する。新規インストールでは `flag_var`
; が未設定のままなので何も起動しない（「収集を勝手に始めない」方針）。
!macro StartServiceIfStopped service_name flag_var
  ${If} ${flag_var} == "1"
    DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: 上書き前に稼働していた ${service_name} サービスを再開します..."
    nsExec::ExecToStack 'sc start "${service_name}"'
    Pop $0
  ${EndIf}
!macroend

; `StartServiceIfStopped` と同じ条件（`flag_var` が "1"）で `sc start` し、
; **RUNNING になるまで最大 30 秒待って結果を返す**版（ChronoGazer が使う。
; #972 のオーナーレビュー: 再開の失敗を成功扱いにしない）。既存の
; `StartServiceIfStopped`（結果を見ない。Hub が使う）の挙動は変えない。
;   結果は $0: "0" = RUNNING になった（または `flag_var` が "1" でなく何もしなかった）、
;   それ以外 = 失敗（`sc start` の終了コード、開始した直後に STOPPED に戻った
;   "stopped"、30 秒で RUNNING にならない "timeout"）。$1 も壊す。
;   試みた後は `flag_var` を "0" に戻す（同じインストーラの別の道 - 中止の後の
;   後始末など - でもう一度起動しない）。失敗しても Abort はしない（判断は
;   呼ぶ側）。ラベルは `${__COUNTER__}`（別のマクロの中から同じ行で 2 回
;   挿入されても衝突しない）。
!macro StartServiceIfStoppedAndWait service_name flag_var
  !define StartSvcID ${__COUNTER__}

  StrCpy $0 "0"
  ${If} ${flag_var} == "1"
    StrCpy ${flag_var} "0"
    DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: 上書き前に稼働していた ${service_name} サービスを再開します..."
    nsExec::ExecToStack 'sc start "${service_name}"'
    Pop $0
    ${If} $0 != "0"
      DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: sc start (${service_name}) が失敗しました（終了コード $0）"
      Goto startsvc_done_${StartSvcID}
    ${EndIf}

    StrCpy $StopWaitCounter 0
    startsvc_wait_${StartSvcID}:
      nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"RUNNING" >nul'
      Pop $0
      ${If} $0 == "0"
        DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: ${service_name} サービスは RUNNING になりました。"
        Goto startsvc_done_${StartSvcID}
      ${EndIf}
      ; sc start が通った直後は START_PENDING のはずなので、STOPPED に戻って
      ; いれば開始に失敗して終わったと見る（最初の 1 秒は見ない）。
      ${If} $StopWaitCounter > 0
        nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"STOPPED" >nul'
        Pop $1
        ${If} $1 == "0"
          StrCpy $0 "stopped"
          DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: ${service_name} サービスは開始した直後に止まりました（${SERVICE_COMMON_OPS_DOC}）"
          Goto startsvc_done_${StartSvcID}
        ${EndIf}
      ${EndIf}
      IntOp $StopWaitCounter $StopWaitCounter + 1
      ${If} $StopWaitCounter >= 30
        StrCpy $0 "timeout"
        DetailPrint "${SERVICE_COMMON_LOG_PREFIX}: (${service_name}) の開始待ちが30秒でタイムアウトしました。手動で状態を確認してください（${SERVICE_COMMON_OPS_DOC}）"
        Goto startsvc_done_${StartSvcID}
      ${EndIf}
      Sleep 1000
      Goto startsvc_wait_${StartSvcID}
    startsvc_done_${StartSvcID}:
  ${EndIf}

  !undef StartSvcID
!macroend

!endif
