; banto-hub インストーラ post-install / pre-uninstall フック（T5-2、
; docs/t5-handoff.md §3・docs/banto-hub-operations.md §10）。
;
; tauri-bundler の NSIS バンドラ（NsisSettings::installer_hooks、
; apps/banto-hub/installer/src/main.rs 参照）が対応する4フックのうち、
; このファイルは NSIS_HOOK_POSTINSTALL と NSIS_HOOK_PREUNINSTALL の2つだけ
; を実装する - T5-1 の `banto-hub.exe install`/`uninstall` サブコマンドを
; 呼ぶだけなので、ファイルコピーの前後どちらであっても意味は変わらず、
; PREINSTALL/POSTUNINSTALL フックは使わない。
;
; ${MAINBINARYNAME}（"banto-hub"）と $INSTDIR は、tauri-bundler
; 2.9.4 の installer.nsi テンプレート側で Section Install /
; Section Uninstall 内の該当箇所まで展開済みの変数としてこの位置で
; 参照できる（ソース確認済み: crates/tauri-bundler/src/bundle/windows/
; nsis/installer.nsi のタグ tauri-bundler-v2.9.4）。
;
; このインストーラ自体を「PerMachine」モードでビルドしている
; （apps/banto-hub/installer/src/main.rs の NsisSettings::install_mode）
; ため、UAC 昇格済みで実行されており、post-install/pre-uninstall の
; このフックも常に管理者権限で動く - `banto-hub.exe install`/`uninstall`
; が要求する管理者権限をここで満たせる。
;
; サービス登録に失敗しても（例: 二重インストール等で `install` が
; べき等でないため既存サービスがあるとエラーになる）、インストーラ本体は
; 中断させない - DetailPrint で案内するだけに留め、必要なら
; docs/banto-hub-operations.md §10 の手順で手動対応してもらう
; （T5-2 実装指示: 無理に完璧を狙わず、失敗時は運用手順に落とす）。

!include "LogicLib.nsh"

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "banto-hub: Windows サービス (BantoHub) を登録しています..."
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" install' $0
  ${If} $0 != 0
    DetailPrint "banto-hub: サービス登録に失敗しました（終了コード $0）。インストール後に管理者権限の PowerShell から手動で次を実行してください: `$INSTDIR\${MAINBINARYNAME}.exe install`（docs/banto-hub-operations.md §10 参照）"
  ${Else}
    DetailPrint "banto-hub: サービス (BantoHub) を登録しました。`Start-Service BantoHub` または OS 再起動で開始します。"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "banto-hub: Windows サービス (BantoHub) の登録を解除しています..."
  ; アンインストール時点でサービスが未登録（一度も install していない等）
  ; でもエラーにしない - uninstall サブコマンド自体がサービス未検出を
  ; 通常のエラーメッセージとして返すだけで、アンインストーラの続行は妨げない。
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" uninstall' $0
!macroend
