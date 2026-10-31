; Hub（apps/banto-hub/installer/hooks/service-hooks.nsh）と ChronoGazer
; （apps/chronogazer/installer/hooks/service-hooks.nsh）の両インストーラの NSIS
; フックが共通で使う、**インストール先**に関する部品（#973 PR-3a。元は #972 で
; ChronoGazer のフックに入れたもの。オーナー決定 2026-10-31「基本 Hub と同じで」）。
;
;   RequireDefaultInstallDir  インストール先の固定。$INSTDIR の本当の置き場
;                             （`..` を畳み、ジャンクション・シンボリックリンクを
;                             解いた実パス）が既定の場所（解かずに畳んだだけ）と
;                             書いたとおりに等しいときだけ通し、通れば $INSTDIR を
;                             畳んだ形に揃える。違えば案内して `Abort`。
;   ResetInstallDirAcl        既存のインストール先の所有者と ACL を、ファイルを置く
;                             前に既定へ戻す（`icacls /setowner` と `/reset`、/L 付き・
;                             /T なし、インストール先そのものと直下の普通のファイル
;                             だけ）。再解析ポイント・サブフォルダ・icacls の失敗は
;                             案内して `Abort`。
;
; 使い方: include する側のフックが、このファイルを `!include` する**前に**次を
; `!define` しておく（service-common.nsh と同じ流儀。ChronoGazer の生成結果の
; 文言を 1 文字も変えないよう、製品固有の文言は引数ではなく定数で受ける）。
;   INSTALL_DIR_COMMON_LOG_PREFIX     DetailPrint の接頭辞（例: "chronogazer"）
;   INSTALL_DIR_COMMON_PRODUCT        案内に出す製品名（例: "ChronoGazer"）
;   INSTALL_DIR_COMMON_DEFAULT_DIR    既定の場所（例: "$PROGRAMFILES64\ChronoGazer"。
;                                     実行時に展開される NSIS の定数を含んでよい）
;   INSTALL_DIR_COMMON_SERVICE_NOUN   サービスとして置くものの呼び名（例:
;                                     "記録エンジン"。「${…}のサービスを登録できない」
;                                     「${…}は、…この場所に置かないと」の形で使う）
;   INSTALL_DIR_COMMON_ACL_ABORT_NOTE ACL を戻せずに中止するときの案内の末尾の 1 文
;                                     （例: "上書き前に止めた ChronoGazer サービスは
;                                     戻します。"。止めたサービスをどう戻すかは製品
;                                     ごとに違うため）
; 任意: マクロ INSTALL_DIR_COMMON_ON_ACL_ABORT が定義されていれば、ResetInstallDirAcl
; の中止の道で `Abort` の直前に挿入する（`.onInstFailed` を持たない Hub が、止めた
; サービスをここで戻すために使う。ChronoGazer は `.onInstFailed` が戻すので定義しない）。
;
; どちらのマクロも $0〜$5 を壊す（呼び出し側はこの後に自分で入れ直す）。
; RequireDefaultInstallDir はファイルを変える前（ChronoGazer は事前の確かめの先頭、
; Hub は PREINSTALL の先頭）に、ResetInstallDirAcl は `File` の直前（ChronoGazer は
; テンプレートの当て直しが `ClearErrors` の前に挿入、Hub は PREINSTALL の末尾）に置く。

!ifndef INSTALL_DIR_COMMON_NSH
!define INSTALL_DIR_COMMON_NSH

!ifndef INSTALL_DIR_COMMON_LOG_PREFIX
  !error "install-dir-common.nsh: include の前に INSTALL_DIR_COMMON_LOG_PREFIX を !define してください"
!endif
!ifndef INSTALL_DIR_COMMON_PRODUCT
  !error "install-dir-common.nsh: include の前に INSTALL_DIR_COMMON_PRODUCT を !define してください"
!endif
!ifndef INSTALL_DIR_COMMON_DEFAULT_DIR
  !error "install-dir-common.nsh: include の前に INSTALL_DIR_COMMON_DEFAULT_DIR を !define してください"
!endif
!ifndef INSTALL_DIR_COMMON_SERVICE_NOUN
  !error "install-dir-common.nsh: include の前に INSTALL_DIR_COMMON_SERVICE_NOUN を !define してください"
!endif
!ifndef INSTALL_DIR_COMMON_ACL_ABORT_NOTE
  !error "install-dir-common.nsh: include の前に INSTALL_DIR_COMMON_ACL_ABORT_NOTE を !define してください"
!endif

!include "LogicLib.nsh"

; --- インストール先の固定 -------------------------------------------------

; $INSTDIR が既定の場所 `${INSTALL_DIR_COMMON_DEFAULT_DIR}` **そのもの**でなければ
; 止める。NSIS の `==` は大文字小文字を区別しない。
;
; 「Program Files（64 ビット・x86）の中ならどこでも」にはしない: Program Files の中に
; 一般の利用者が書けるサブフォルダ（別のアプリが ACL を緩めたフォルダなど）があると、
; そこを `/D=` で選べば、置いたサービスの exe が LocalSystem のサービスに
; 登録されたうえで後から差し替えられる（#972 のオーナーレビュー（2b9ad63））。
; 既定の場所は親（Program Files）から SYSTEM・Administrators・TrustedInstaller だけが
; 書ける ACL を継ぐ。既存のフォルダの ACL は、ファイルを置く前に
; ResetInstallDirAcl が既定へ戻す。elev 側の ACL の監査は #973 で別に検討する。
;
; 文字列だけで見ると `/D=C:\Program Files\..\Users\Public\<製品>` や
; Program Files の中のジャンクションが通り、実際の置き場が外でも、ファイルを
; 写した後の service-install まで気付けない（#972 のオーナーレビュー（7f86c38））。
; そこで、ファイルを変える前のここで**本当の置き場**を比べる: $INSTDIR を
; InstallDirResolveRealPath（下。`..` の正規化 + ジャンクションを解いた実パス。
; まだ無いフォルダは、ある親まで解いて残りを継ぎ足す）にし、既定の場所は**解かずに**
; InstallDirNormalizePath（`..` などを畳むだけ）にして、全体が**書いたとおりに**
; 等しいかを見る。実パスは存在する全ての構成要素を通して解くので、既定の場所
; かその上のフォルダがジャンクション／シンボリックリンクだと実パスが書いた
; 既定の場所と食い違い、止まる（Program Files 自体が付け替えられていても同じ。
; 両方を解いて比べるとユーザーが書ける場所へのジャンクションが通ってしまう。
; #972 のオーナーレビュー（f9d858b））。実パスが求まらなければ（API の失敗・
; ネットワークパスなど）「違う」として止める（安全側）。
;
; 通ったときは $INSTDIR を既定の場所の書き方（`${INSTALL_DIR_COMMON_DEFAULT_DIR}` を
; `..`・`.`・重なった区切りを畳んだ形）に置き換える。後の確かめ（サービスの
; ImagePath や InstallLocation との比較）と、テンプレートが書く InstallLocation・
; elev が登録する exe のパスが同じ表記になるようにするため。このマクロは事前の
; 確かめの先頭に置くこと。**`SetOutPath $INSTDIR` の後に呼ぶ側（Hub の PREINSTALL）
; は、このマクロの後にもう一度 `SetOutPath $INSTDIR` すること**（`File` は $OUTDIR
; に書く。同じ実体を指すが、表記を揃える）。
;
; $0〜$2 を壊す（呼び出し側はこの後に自分で入れ直す）。
!macro RequireDefaultInstallDir
  Push "$INSTDIR"
  Call InstallDirResolveRealPath
  Pop $0
  Push "${INSTALL_DIR_COMMON_DEFAULT_DIR}"
  Call InstallDirNormalizePath
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
    DetailPrint "${INSTALL_DIR_COMMON_LOG_PREFIX}: インストール先 $INSTDIR（実際の場所: $0）は既定の場所 (${INSTALL_DIR_COMMON_DEFAULT_DIR}) ではありません（既定の場所かその上のフォルダが、別の場所へのジャンクションやリンクになっている場合も含みます）。${INSTALL_DIR_COMMON_SERVICE_NOUN}のサービスを登録できないため、インストールを中止しました。"
    MessageBox MB_OK|MB_ICONSTOP "インストール先 ($INSTDIR) が既定の場所ではありません。$\r$\n実際の場所: $0$\r$\n$\r$\n${INSTALL_DIR_COMMON_PRODUCT} は既定の場所 (${INSTALL_DIR_COMMON_DEFAULT_DIR}) にしかインストールできません（既定の場所かその上のフォルダが、別の場所へのジャンクションやリンクになっている場合も含みます）。${INSTALL_DIR_COMMON_SERVICE_NOUN}は、一般の利用者が書き換えられないこの場所に置かないとサービスとして登録できません。インストール先を変えずにインストールし直してください。$\r$\n$\r$\nまだ何も変更していません。" /SD IDOK
    Abort "インストール先が既定の場所ではないため、インストールを中止しました。"
  ${EndIf}
  ${If} $2 != "$INSTDIR"
    DetailPrint "${INSTALL_DIR_COMMON_LOG_PREFIX}: インストール先 $INSTDIR を $2 として扱います。"
    StrCpy $INSTDIR $2
  ${EndIf}
!macroend

; パスを `kernel32::GetFullPathNameW` で正規化する（`..`・`.`・重なった区切りを
; 畳む。ジャンクションは解かない）。末尾の `\` は取る（ルート `C:\` を除く）。
;   入力: スタックの先頭にパス。出力: スタックの先頭に正規化したパス。失敗
;   （API の失敗・${NSIS_MAX_STRLEN} に収まらない・`\\` で始まる UNC／`\\?\`）
;   なら ""。$0〜$3 は保存して戻す。
Function InstallDirNormalizePath
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
Function InstallDirResolveRealPath
  Exch $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  ; $1 = 存在する親（正規化したパスから、無い末尾の部分を 1 つずつ外す）、
  ; $2 = 外した部分（`\a\b` の形で前に積む）、$0 = 結果。
  Push $0
  Call InstallDirNormalizePath
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

; --- 所有者と ACL の既定への戻し ----------------------------------------------

; #972 のオーナーレビュー（2b9ad63）: インストール先が Program Files の中で既定の
; 場所だけに限っても、管理者が弱い ACL で先に作った既定の場所のフォルダが残っていると、
; 一般の利用者が LocalSystem のサービスの exe を差し替えられる。
; そこで、ファイルを置く前（`File` の直前）に、所有者を BUILTIN\Administrators
; （S-1-5-32-544）にし、明示の ACE を捨てて Program Files から継承する ACL だけにする。
; どれかが駄目なら案内して `Abort`（まだファイルは変えていない。止めたサービスは、
; ChronoGazer は .onInstFailed が、Hub は INSTALL_DIR_COMMON_ON_ACL_ABORT が戻す）。
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
;       - フォルダ → 案内して `Abort`。どちらのインストーラもインストール先に
;         サブフォルダを作らない（tauri-bundler のテンプレートは main・外部バイナリの
;         `File`・`$INSTDIR` 直下の resources と `uninstall.exe` の書き込みだけ）ので、
;         あれば利用者か別のプログラムが置いた物。再帰で辿る代わりに、消してから
;         （またはアンインストールしてから）やり直してもらう。
;       - 普通のファイル → `/setowner` と `/reset` を /L 付き・/T なしで掛ける。
;         残っているファイルも既定の ACL に戻すのは、サービスの exe の隣に
;         利用者が書ける DLL が残ると、LocalSystem のサービスが DLL の検索順で
;         それを読み込みうるため（置き換えでなく「残ったまま」でも危ない）。
;   (a) の後は一般の利用者が直下に項目を足せないので、(b) で見た項目が icacls までの
;   間に別の物へ差し替わることはない（自分の項目を消すことはできるが、足せない）。
;
; $0〜$5 を壊す（$2 は FindFirst のハンドル。中止の道は全部 FindClose してから `Abort`）。
!macro ResetInstallDirAcl
  ; (a) インストール先そのもの。
  StrCpy $4 "$INSTDIR"
  System::Call 'kernel32::GetFileAttributesW(w r4) i .r5'
  IntOp $0 $5 & 1024
  ${If} $5 == -1
  ${OrIf} $0 != 0
    !insertmacro AbortInstallDirAcl "インストール先そのものが再解析ポイント（ジャンクションやリンク）になっているか、属性を読めませんでした。"
  ${EndIf}
  !insertmacro IcaclsResetInstallDirEntry
  ${If} $0 != "0"
    DetailPrint "${INSTALL_DIR_COMMON_LOG_PREFIX}: icacls の出力: $1"
    !insertmacro AbortInstallDirAcl "icacls が失敗しました（終了コード $0）。"
  ${EndIf}
  ; (b) 直下の項目。
  FindFirst $2 $3 "$INSTDIR\*.*"
  ${If} $2 == ""
    !insertmacro AbortInstallDirAcl "インストール先の中身を列挙できませんでした。"
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
        !insertmacro AbortInstallDirAcl "インストール先の中に再解析ポイント（ジャンクションやリンク）があるか、属性を読めませんでした。"
      ${ElseIf} $1 != 0
        FindClose $2
        !insertmacro AbortInstallDirAcl "インストール先の中にサブフォルダがあります。${INSTALL_DIR_COMMON_PRODUCT} のインストーラはサブフォルダを作らないので、${INSTALL_DIR_COMMON_PRODUCT} の物ではありません。"
      ${EndIf}
      !insertmacro IcaclsResetInstallDirEntry
      ${If} $0 != "0"
        DetailPrint "${INSTALL_DIR_COMMON_LOG_PREFIX}: icacls の出力: $1"
        FindClose $2
        !insertmacro AbortInstallDirAcl "icacls が失敗しました（終了コード $0）。"
      ${EndIf}
    ${EndIf}
    FindNext $2 $3
  ${Loop}
  FindClose $2
!macroend

; ResetInstallDirAcl の 1 項目分: $4 のパス（インストール先そのものか、
; その直下の普通のファイル）の所有者を Administrators にし、明示の ACE を捨てる。
; /L でリンクを解かず、/T を付けず（その項目だけ）。$0 に終了コード、$1 に出力。
!macro IcaclsResetInstallDirEntry
  nsExec::ExecToStack '"$SYSDIR\icacls.exe" "$4" /setowner *S-1-5-32-544 /L /C /Q'
  Pop $0
  Pop $1
  ${If} $0 == "0"
    nsExec::ExecToStack '"$SYSDIR\icacls.exe" "$4" /reset /L /C /Q'
    Pop $0
    Pop $1
  ${EndIf}
!macroend

; ResetInstallDirAcl の中止: $4 の項目について `reason` を案内して `Abort`。
; まだファイルは変えていない。止めたサービスの戻し方は製品ごと
; （INSTALL_DIR_COMMON_ACL_ABORT_NOTE・INSTALL_DIR_COMMON_ON_ACL_ABORT）。
; 呼ぶ前に FindClose を済ませること。
!macro AbortInstallDirAcl reason
  DetailPrint "${INSTALL_DIR_COMMON_LOG_PREFIX}: インストール先 $INSTDIR の所有者・ACL を既定に戻せませんでした。${reason}（対象: $4）"
  MessageBox MB_OK|MB_ICONSTOP "インストール先 $INSTDIR の所有者・アクセス権を既定に戻せませんでした。$\r$\n${reason}$\r$\n対象: $4$\r$\n$\r$\n既存のフォルダのアクセス権が弱いまま実行ファイルを置かないよう、インストールを中止しました。ファイルはまだ変更していません。${INSTALL_DIR_COMMON_ACL_ABORT_NOTE}$\r$\n$\r$\n対象のリンクやサブフォルダを取り除く（または先にアンインストールする）か、フォルダを使用しているプログラムを終了するか、管理者権限で $INSTDIR の所有者とアクセス権を確かめてから、もう一度実行してください。" /SD IDOK
  !ifmacrodef INSTALL_DIR_COMMON_ON_ACL_ABORT
    !insertmacro INSTALL_DIR_COMMON_ON_ACL_ABORT
  !endif
  Abort "インストール先の所有者・ACL を既定に戻せなかったため、インストールを中止しました。"
!macroend

!endif ; INSTALL_DIR_COMMON_NSH
