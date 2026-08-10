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
; サービス登録に失敗しても、インストーラ本体は中断させない - DetailPrint
; で案内するだけに留め、必要なら docs/banto-hub-operations.md §10 の手順で
; 手動対応してもらう（T5-2 実装指示: 無理に完璧を狙わず、失敗時は運用
; 手順に落とす）。T17-4（docs/banto-hub-t17-design.md §11）以降、
; `banto-hub.exe install` は既に同名サービスが登録済みの場合（アップ
; グレード時等）は既存設定を変更せず正常終了（終了コード0）するため、
; ここで失敗として案内されるのは「本当に登録に失敗した」場合のみになった
; （既存サービスがあるだけでは失敗しない）。
;
; T17-4（P4「Demand 化」）: 新規インストールの既定起動種別は手動
; （Demand）になった - OS 再起動だけではサービスは開始しない。

!include "LogicLib.nsh"

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "banto-hub: Windows サービス (BantoHub) を登録しています..."
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" install' $0
  ${If} $0 != 0
    DetailPrint "banto-hub: サービス登録に失敗しました（終了コード $0）。インストール後に管理者権限の PowerShell から手動で次を実行してください: `$INSTDIR\${MAINBINARYNAME}.exe install`（docs/banto-hub-operations.md §10 参照）"
  ${Else}
    DetailPrint "banto-hub: サービス (BantoHub) の登録を確認しました。起動種別は手動（Demand）です - OS 再起動では開始しません。`Start-Service BantoHub` または管理 UI から明示的に開始してください（既存インストールの場合は起動種別を変更していません）。"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "banto-hub: Windows サービス (BantoHub) の登録を解除しています..."
  ; アンインストール時点でサービスが未登録（一度も install していない等）
  ; でもエラーにしない - uninstall サブコマンド自体がサービス未検出を
  ; 通常のエラーメッセージとして返すだけで、アンインストーラの続行は妨げない。
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" uninstall' $0
!macroend
