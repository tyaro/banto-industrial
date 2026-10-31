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
; トール」節参照）。フックのほかに、当て直した NSIS テンプレート
; （src/main.rs の TEMPLATE_PATCHES、#973 PR-3b）が Section WebView2 の先頭・
; `.onInstFailed`・`File` の直後・Section Uninstall の途中・`un.onUninstFailed`
; からこのファイルのマクロ（BantoHubInstallPreflight など、下の「フック本体」）
; を呼ぶ。
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
; **サブコマンド（service-install など）の失敗ではインストーラ本体を中断させない**
; - DetailPrint で案内するだけに留め、必要なら docs/banto-hub-operations.md §12
; の手順で手動対応してもらう（T5-2 以来の方針）。**例外**は ChronoGazer（#972）に
; そろえた次の 6 つで、どれも「置き場・ファイルの状態が壊れたまま成功に見える」
; のを防ぐためのもの（#973 PR-3a・PR-3b）:
;   - 事前の確かめ（下の「事前の確かめ」節）: 置き場が既定の場所でない・所有者と
;     ACL を既定に戻せないときは、何も変えずに `Abort`（置いてからでは LocalSystem
;     のサービスの exe を一般の利用者が差し替えられる状態を作ってしまうため）。
;   - サービスが止まり切らない（RequireBantoHubServiceInactive）: 実行ファイルを
;     入れ替えられないので `Abort`。
;   - `File` の失敗（RequireBantoHubFilesCopied）: 旧い exe と新しい exe が混ざった
;     まま POSTINSTALL に進まない。
;   - アンインストールで exe が使用中（RequireBantoHubExesUnlocked）・消し残った
;     （RequireBantoHubExesDeleted）: 登録だけ消えて exe が残る状態を作らない。
;   - アンインストールでサービスの登録を解除できない（UnregisterBantoHubServices）:
;     ファイルを消す前に `Abort` し、登録が残ったまま ImagePath の exe が消える状態を
;     作らない（#977 の指摘 P2）。解除が 0 で返っても削除の印だけ付いて登録が残る
;     「削除の保留」（`DeleteFlag`=1）も同じ扱い（RequireBantoHubServiceDeleteNotPending。
;     保留の登録は戻しも再開もしない）。
;   途中で止めるときは、各中止の道が案内の `MessageBox` と `Abort` より前に、上書き前に
;   止めたサービスを戻す（RestoreStoppedBantoHubServices、下の節）。フックが割り込め
;   ない 2 つの道だけ `.onInstFailed`／`un.onUninstFailed` が戻す。
;
; ## 事前の確かめ（BantoHubInstallPreflight、Section WebView2 の先頭）
;
; テンプレートの Section は EarlyChecks → WebView2（ランタイムが無ければ取得して
; 入れる）→ Install（`SetOutPath` → PREINSTALL フック → `File`）の順。当て直し
; （src/main.rs の P3）が `Section WebView2` の先頭に挿入するこのマクロが、
; **何も変えずに止められる確かめ**を WebView2 の取得と `SetOutPath`（空の
; インストール先の作成）より前に済ませる（#973 PR-3a の Codex の指摘 1: 当て直し
; なしでは確かめが WebView2 の後になり、駄目でも WebView2 だけ入り、空のフォルダが
; できていた）。ここで `Abort` すると Section WebView2 ごと止まり、Install には
; 進まない。まだ `$INSTDIR` は作られておらず、サービスもシェルも止めていない。
;
;   (0) **インストール先の固定**（共通部品 RequireDefaultInstallDir、#973 PR-3a、
;       オーナー決定 2026-10-31「基本 Hub と同じで」）: $INSTDIR の本当の置き場
;       （`..` を畳み、ジャンクション・シンボリックリンクを解いた実パス）が
;       `$PROGRAMFILES64\BantoHub` と書いたとおりに等しいときだけ通す。ディレクトリ
;       の頁や `/D=` で別の場所を選ばれると、elev は Program Files の外の exe を
;       サービスに登録しない（権限の昇格を防ぐため）ので、サービスの無いインストール
;       が黙ってできてしまう。Program Files の中の一般の利用者が書けるサブフォルダや、
;       Program Files の外へのジャンクションも、文字列ではなく実パスで見て断る
;       （#972 のオーナーレビュー（2b9ad63・7f86c38・f9d858b））。通れば $INSTDIR を
;       畳んだ形に揃える（テンプレートの `SetOutPath $INSTDIR` はこの後なので、
;       やり直しは要らない）。
;   (1) **インストール先の所有者・ACL の既定への戻し**（共通部品 ResetInstallDirAcl、
;       #973 PR-3a）: 管理者が弱い ACL で先に作った `C:\Program Files\BantoHub` が
;       残っていると、一般の利用者が LocalSystem のサービスの exe を差し替えられる
;       （#972 のオーナーレビュー（2b9ad63））。`icacls /setowner *S-1-5-32-544` と
;       `/reset` を、インストール先そのものと直下の普通のファイルだけに /L 付き・
;       /T なしで掛ける（再解析ポイント・サブフォルダは `Abort`。このインストーラも
;       サブフォルダを作らない - resources は `$INSTDIR` 直下）。**インストール先が
;       まだ無ければ飛ばす**（新規インストール。`SetOutPath` が Program Files から
;       既定の ACL を継いで作る）。**サービスとシェルを止める前、レジストリを触る
;       前に置く**（PR-3a のオーナーのレビュー。PR-3b で WebView2 の前へ）: 中止で
;       戻すものが無い。使用中の exe でも所有者・DACL は書き換えられる
;       （WRITE_OWNER・WRITE_DAC は共有モード FILE_SHARE_* の対象外。実行中イメージ
;       の排他は書込み・削除の拒否だけ）。戻した後は一般の利用者が直下に手を出せない
;       ので、ここから `File` までの間が空いても穴は開かない。
;
; ## PREINSTALL（§4.3 表・§7 決定4「自動で停止し、動いていたものだけ再開」）
;
; 当て直したテンプレートの Section Install は `SetOutPath $INSTDIR` → **このフック**
; → CheckIfAppIsRunning → `ClearErrors` → `File` → RequireBantoHubFilesCopied →
; POSTINSTALL の順。このフックが「ファイルを変える前」の最後の場所になる。
;
;   (2) alpha.2 の Hub 単体インストーラからの上書きに備えた MainBinaryName の
;       先読み（下の節）。
;   (3) BantoHubSink が Running なら停止し、$SinkWasRunning に "1" を記録
;   (4) BantoHub が Running なら停止し、$BantoHubWasRunning に "1" を記録
;   (5) どちらかが RUNNING／*_PENDING のままなら案内して `Abort`
;       （RequireBantoHubServiceInactive、#973 PR-3b。ChronoGazer の同名の確かめと
;       同じ形）。StopServiceIfRunning は 30 秒で止まり切らなくても案内して続行する
;       ので、そのままだと `File` が使用中で失敗する。自分で止めたサービスなら
;       さらに最大 60 秒だけ停止の完了を待ち、STOPPED になれば続行する。止まり
;       切らなければそのサービスのフラグを "0" にして（戻す操作が無い）、もう片方の
;       止めたサービスを案内より前に戻してから `Abort`。
;   (6) banto-hub-shell.exe が動いていれば taskkill で終了する
; 新規インストール（サービス未登録、`sc query` が失敗する = 1060 相当）は、
; RUNNING の文字列がそもそも見つからないため何も起きず静かに素通りする -
; 「見つからない」と「止まっている」を区別する必要はない（どちらも「今回は
; 何もしない」で同じ扱いでよい）。
;
; テンプレートの `CheckIfAppIsRunning`（フックの直後。シェルが残っていれば案内の後に
; KillProcess）はそのまま使う（ChronoGazer は「強制終了しない」約束のために
; 置き換えたが、Hub のシェルには終了を頼む道が無く、(6) の taskkill と同じ方針）。
;
; ## ファイルの上書きの結果（RequireBantoHubFilesCopied、#973 PR-3b）
;
; 原本のテンプレートは、上書きの `File` が使用中で書けないと中止／再試行／無視の窓を
; 出し、「無視」はエラーフラグを立てて続行する。フラグは誰も見ず、旧い exe と新しい
; exe が混ざったまま POSTINSTALL を通って成功と報告される。当て直し（P5）で
; `AllowSkipFiles off`（「無視」を出さない。silent では中止）、`File` の直前に
; `ClearErrors`、外部バイナリの `File` の直後にこのマクロを置く: フラグが立って
; いれば、止めたサービスを戻してから案内して `Abort`（POSTINSTALL には進まない）。
; 各 exe は丸ごと旧いか丸ごと新しいかのどちらかで壊れてはいない。
;
; ## 中止の道での止めたサービスの戻し（RestoreStoppedBantoHubServices、#973 PR-3b）
;
; UI では、Section の `Abort` の後、利用者が失敗の頁で [キャンセル] を押すまで
; `.onInstFailed`／`un.onUninstFailed` は呼ばれない（NSIS の Ui.c。失敗時は autoclose も
; 効かない）。戻しをコールバックだけに任せると、頁を開いたままの間 BantoHub・
; BantoHubSink が止まったままになる（#977 のオーナーの Codex CLI レビューの P2）。
; そこで**フックの各中止の道は、案内の `MessageBox` と `Abort` より前に
; RestoreStoppedBantoHubServices を呼ぶ**: PREINSTALL／PREUNINSTALL で止めてまだ
; 戻していないサービス（フラグが "1"）を Hub → Sink の順に `sc start` し、RUNNING まで
; 待つ（共通部品 StartServiceIfStoppedAndWait）。結果は文で $ServiceRestoreNote に
; 返し、中止の案内の MessageBox に含める（戻しの案内を別の窓で二重に出さない。
; 戻せなかったサービスの名前は $ServiceRestoreFailed にも入る）。戻さないと決めた道
; （止まり切らなかったサービス、登録を戻せなかったサービス）は、呼ぶ前にフラグを
; "0" にしておく。
;
; `.onInstFailed`／`un.onUninstFailed`（原本に無い。当て直し P4 が足す）は補助で、
; フックが割り込めない 2 つの道 - `File` の「使用中」の窓の [中止]（silent では自動の
; 中止）と、テンプレートの `CheckIfAppIsRunning` の [キャンセル] - のために残す。
; 共通部品がフラグを "0" に戻すので、フックの道で戻した後に呼ばれても何もしない
; （二重に起動しない）。戻せなかったときだけ、ここで案内の窓を出す（他に窓が無い）。
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
; (4) `banto-hub-elev.exe prepare-sink-config`（#630 PR-2、2026-10-17 オーナー
;     決定）: サイドカーの設定ファイル（Hub の API キー入り）の置き場
;     `%ProgramData%\BantoHub\sink\` を、確かめ終えてから SYSTEM・Administrators
;     だけ・所有者 Administrators の保護された DACL で作る（あれば整える）。中に
;     本物の `banto-hub-sink.toml` があれば所有者と ACL を整え直し（中身は読まない・
;     変えない）、`banto-hub-sink.toml.example`（exe の隣の同梱物）をインストールの
;     たびに差し替える。**実ファイルは作らない** - API キーは Hub を起動してから
;     発行するもの（§4.4）。以前の NSIS の `CreateDirectory`/`CopyFiles` は継承の
;     DACL（Users が読める）で作ってしまうので使わない。ルート直下の旧い
;     `.example`・旧い toml には触らない（旧い toml があると sink は起動しない -
;     docs/banto-hub-operations.md §12「サイドカーの初回セットアップ」）。sink の `install`・
;     `grant-service-acl` の後、サービスの再開の前に呼ぶ（再開した sink が新しい
;     置き場を読むように）。
; (5) PREINSTALL で止めたものだけ `sc start` で再開する（Hub → Sink の順、
;     §4.3「起動していたものだけ再開」）。**新規インストールでは
;     $BantoHubWasRunning/$SinkWasRunning が未設定のままなので何も
;     起動しない**（T17-4 の「収集を勝手に始めない」方針を維持）。
;
; いずれのサブコマンドも冪等 - 既に登録済みのサービスに対して `install`/
; `service-install` を呼んでも起動種別・実行ファイル・サービスの ACL は変更しない
; （T17-4、apps/banto-hub/core/src/service_install.rs 参照）。ただし profile の ACL は
; `service-install` のたびに保護された DACL へ書き直す（#626、既存の owner は引き継ぐ。
; profile が使用中のときだけ書き直しを飛ばして 0 で終わる - 警告は標準エラー出力だけで
; インストーラには残らない。docs/banto-hub-operations.md §11）。
; #589 以降、その経路でも PRESHUTDOWN の待ち時間（30 秒）だけは更新し、
; 読み戻して確認する。設定できなかったときも `install` は終了コード 0 で
; 警告を 1 行出すだけ（2026-10-15）- 下の `$0 != 0` の案内には出ず、
; `service-install` は続く Operators・ACL の付与まで進む。その警告は
; 標準エラー出力だけで、`ExecWait` は子の出力を取り込まないので
; インストーラには表示も記録もされない（確認はレジストリの
; `PreshutdownTimeout`、docs/banto-hub-operations.md §11）。
;
; (6) デスクトップショートカットの削除（I4 追従、
;     docs/banto-hub-installer-design.md §7-3「デスクトップショートカット
;     は作らない」・§8.2）。tauri-bundler 2.9.4 の installer.nsi テンプレ
;     ートは `$DESKTOP\${PRODUCTNAME}.lnk` を2経路で作る（ソース確認済み）:
;     (a) silent / passive（`/S` `/P`）では Section Install 内の
;         CreateOrUpdateDesktopShortcut を**無条件**に呼ぶ（行 726-731、
;         POSTINSTALL 挿入位置 733-734 より前）。
;     (b) 対話モードでは完了ページの「デスクトップにショートカットを作成」
;         チェック（MUI_FINISHPAGE_SHOWREADME、既定 ON）で、Section の
;         **後**に利用者操作で作る - フックからは介入できない。
;     よってこのフック（Section 末尾）では (a) の分だけ削除する。(b) は
;     利用者の選択に委ねる（設計 §7-3 の補足、§8.2）。PerMachine では
;     `SetShellVarContext all` により `$DESKTOP` はパブリックデスクトップ。
;
; ## PREUNINSTALL と Section Uninstall の順（§4.3 表・§7 決定7、#973 PR-3b）
;
; 当て直した Section Uninstall の順: **このフック**（シェルの終了 → Sink → Hub の
; 停止 → 止まったことの確かめ）→ CheckIfAppIsRunning（テンプレート）→
; RequireBantoHubExesUnlocked → UnregisterBantoHubServices → `Delete` 4 exe と
; resources → RequireBantoHubExesDeleted → `uninstall.exe` の `Delete` →
; ショートカット → Uninstall キーの削除。
;
;   (a) **使用中の exe の確かめ**（RequireBantoHubExesUnlocked。P6 の当て直しが
;       CheckIfAppIsRunning の直後に挿入する）: 4 つの exe のそれぞれを
;       `FileOpen ... a`（読み書きで開き、中身は変えずすぐ閉じる。無いファイルは
;       飛ばす）で開けるかを見る。動いている exe はローダーが書込みの共有を許さない
;       ので開けない。1 つでも開けなければ、止めたサービスを戻してから案内して `Abort`
;       （まだ何も変更していない）。
;   (b) **サービス登録の解除**（UnregisterBantoHubServices。同じ当て直しが (a) の直後
;       に挿入する）: サイドカー→Hub の順に解除する。登録が無ければ飛ばし、登録が
;       あって頼む exe が無ければ `sc delete`（`banto-hub-elev.exe` が無いだけなら
;       `banto-hub.exe uninstall`（同等の処理、apps/banto-hub/host/src/bin/banto_hub/
;       win_service.rs 参照）にフォールバック）。**解除に失敗したらファイルを消さずに
;       案内して `Abort`**（ChronoGazer と同じ。登録が残ったまま ImagePath の exe が
;       消えるのを防ぐ）。Sink の解除が通って Hub の解除が失敗したときは、`Abort` の
;       前に Sink の登録を `install`＋`grant-service-acl` で戻す（(d) と同じ戻し方。
;       戻せなければ $SinkWasRunning を "0" にして案内）。どちらの失敗も、止めたサービス
;       （登録がある分）を戻してから案内する。解除が 0 で返った直後には `DeleteFlag` を
;       見て、別のプロセスがハンドルを持っていて削除が保留になっていれば、同じく
;       ファイルを消さずに `Abort`（そのサービスは戻しも再開もできないのでフラグを "0"
;       に。Hub が保留なら消えた Sink の登録だけ戻す）。PR-3b より前はこのフックの中で
;       解除し、失敗は案内するだけで続行していた（CheckIfAppIsRunning の前）。
;   (d) **消えたことの確かめ**（RequireBantoHubExesDeleted。P6 の当て直しが外部
;       バイナリの `Delete` の直後・`uninstall.exe` の `Delete` と Uninstall キーの
;       削除の前に挿入する）: exe が残っていれば（再確認の後に起動されたシェル・
;       ウイルス対策のロック）、`uninstall.exe` と「アプリ」の登録を消す前に止める。
;       (b) で解除したサービスの登録は、残っている exe で頼める分だけ戻す
;       （`banto-hub-elev.exe` と `banto-hub.exe` が残っていれば `service-install`、
;       `banto-hub-sink.exe` が残っていれば `install` と `grant-service-acl`。起動種別は
;       手動に戻る。(b) で未登録だったものは戻さない）。戻せたサービスだけ、案内の前に
;       再開する。残った exe 以外は消えているが、`uninstall.exe`
;       と「アプリ」の登録は残るので、もう一度アンインストールできる（(a) は無い
;       ファイルを飛ばし、(b) は登録が無ければ飛ばし、登録があって exe が無ければ
;       `sc delete` で解除する）。
;
; `%ProgramData%\BantoHub\`（DB・サイドカー設定）とサービスログは
; **削除しない**（§4.5「アンインストール」・オーナー決定 §7-7 - 再
; インストールで設定・データが戻ることを優先する）。

; --- 共通部品 -----------------------------------------------------------
;
; サービスの停止・シェルの終了・止めたサービスの再開は、ChronoGazer の
; インストーラとも共通の installer-common/service-common.nsh に切り出した
; （#392 B3-4、StopServiceIfRunning・TerminateProcessIfRunning・
; StartServiceIfStopped・StartServiceIfStoppedAndWait。ラベルは `${__LINE__}`／
; `${__COUNTER__}` で一意化）。製品固有の文言は include の前に定数で渡す。
; `BANTO_INSTALLER_COMMON_DIR`（共通ファイルのディレクトリの絶対パス）は、この
; フックを tauri-bundler に渡す前に apps/banto-hub/installer/src/main.rs が先頭へ
; `!define` する（makensis の作業ディレクトリは tauri-bundler の出力先で、相対パスは
; 頼れないため）。
!define SERVICE_COMMON_LOG_PREFIX "banto-hub"
!define SERVICE_COMMON_OPS_DOC "docs/banto-hub-operations.md §12"
!include "${BANTO_INSTALLER_COMMON_DIR}\service-common.nsh"

; インストール先の固定と所有者・ACL の既定への戻し（RequireDefaultInstallDir・
; ResetInstallDirAcl）は ChronoGazer と共通の installer-common/install-dir-common.nsh
; （#973 PR-3a。#972 で ChronoGazer に入れたものを切り出した）。製品固有の文言は
; 同じく include の前に定数で渡す。
!define INSTALL_DIR_COMMON_LOG_PREFIX "banto-hub"
!define INSTALL_DIR_COMMON_PRODUCT "BantoHub"
!define INSTALL_DIR_COMMON_DEFAULT_DIR "$PROGRAMFILES64\BantoHub"
!define INSTALL_DIR_COMMON_SERVICE_NOUN "Hub"
; Hub は ACL の戻しをサービス・シェルを止める前（WebView2 の前）に行うので、中止のときに
; 戻すものが無い。
!define INSTALL_DIR_COMMON_ACL_ABORT_NOTE ""
!include "${BANTO_INSTALLER_COMMON_DIR}\install-dir-common.nsh"

; 30 秒で止まり切らなかったサービスの停止完了を、Abort の前にさらに待つ上限（秒）。
!define BANTO_HUB_SERVICE_STOP_EXTRA_WAIT_SECONDS 60

Var BantoHubWasRunning
Var SinkWasRunning
; アンインストールの (b) でこのアンインストーラが登録を解除したら "1"（未登録で飛ばした
; ときは ""）。(b) の Hub の失敗と (d) の消し残りで、解除した分だけ登録を戻すために見る。
Var BantoHubUnregistered
Var SinkUnregistered
; RestoreStoppedBantoHubServices の結果（上の「中止の道での止めたサービスの戻し」）。
; Note は中止の案内に含める 1 文、Failed は再開できなかったサービスの名前（無ければ ""）。
Var ServiceRestoreNote
Var ServiceRestoreFailed
; RestoreStoppedBantoHubServices が呼び出し側の $0〜$3 を退避する先。nsExec::ExecToStack が
; 出力をスタックに残すので、Push／Pop では保てない。
Var RestoreSaved0
Var RestoreSaved1
Var RestoreSaved2
Var RestoreSaved3

; alpha.2 で公開済みの Hub単体インストーラ（main=`banto-hub.exe`）から I1
; 一体インストーラ（main=`banto-hub-shell.exe`）へ上書きインストールすると
; `banto-hub.exe` が消える経路を塞ぐ（PR #322 レポートで発見）。
;
; tauri-bundler 2.9.4 の installer.nsi テンプレート（crates.io から取得した
; ソースで確認済み: bundle/windows/nsis/installer.nsi、Section Install）は
; File 命令での本体コピー（テンプレート行648）より後、行690-698 で次を行う:
;
;   ; Remove old main binary if it doesn't match new main binary name
;   ReadRegStr $OldMainBinaryName SHCTX "${UNINSTKEY}" "MainBinaryName"
;   ${If} $OldMainBinaryName != ""
;   ${AndIf} $OldMainBinaryName != "${MAINBINARYNAME}.exe"
;     Delete "$INSTDIR\$OldMainBinaryName"
;   ${EndIf}
;
;   ; Save current MAINBINARYNAME for future updates
;   WriteRegStr SHCTX "${UNINSTKEY}" "MainBinaryName" "${MAINBINARYNAME}.exe"
;
; `${UNINSTKEY}` は `Software\Microsoft\Windows\CurrentVersion\Uninstall\
; ${PRODUCTNAME}`（テンプレート行66、このプロジェクトでは "...\Uninstall\
; BantoHub"）、`${MAINBINARYNAME}` は I1 では `banto-hub-shell`
; （main.rs の `SHELL_BINARY_NAME`、テンプレート行52 で埋め込まれる）。
; alpha.2 の Hub単体インストーラはここに "MainBinaryName"="banto-hub.exe"
; を書き込み済みなので、そこから I1 へ上書きすると
; `$OldMainBinaryName`="banto-hub.exe" != "banto-hub-shell.exe" と判定され、
; File 命令でコピーしたばかりの `$INSTDIR\banto-hub.exe`
; （I1 では main ではなく同梱バイナリの1つ）が `Delete` される -
; Hub 本体が消えたまま「インストール完了」になってしまう。
;
; 対策方式: `NSIS_HOOK_PREINSTALL` はテンプレートの File コピー・上記削除
; 判定（テンプレート行641-643 で挿入 → 648 → 690-698、いずれも
; NSIS_HOOK_PREINSTALL より後）より**前**に実行される。ここでレジストリの
; "MainBinaryName" を先読みし、旧 Hub単体インストーラの値 "banto-hub.exe"
; と一致する場合は新 main の値 "banto-hub-shell.exe" に書き換えてしまう
; ことで、テンプレート側の
; `$OldMainBinaryName != "${MAINBINARYNAME}.exe"` 判定を先に「一致」に
; させ、削除処理そのものをスキップさせる（POSTINSTALL でテンプレートが
; 改めて同じ値を書き直すだけなので副作用は無い）。
; ファイルコピー後に走る削除自体をフックから直接止める手段は無い
; （NSIS_HOOK_POSTINSTALL はテンプレート行733-734 挿入 = 削除判定より
; さらに後なので手遅れ）ため、判定の入力（レジストリ値）を先に無害化する
; このレジストリ先読み方式を選んだ。
!macro FixupHubOnlyInstallerMainBinaryName
  ReadRegStr $0 SHCTX "${UNINSTKEY}" "MainBinaryName"
  ${If} $0 == "banto-hub.exe"
    DetailPrint "banto-hub: alpha.2 の Hub単体インストーラからの上書きを検出しました。banto-hub.exe が誤って削除されないようレジストリの MainBinaryName を更新します..."
    WriteRegStr SHCTX "${UNINSTKEY}" "MainBinaryName" "banto-hub-shell.exe"
  ${EndIf}
!macroend

; PREINSTALL (5)／PREUNINSTALL: `service_name` が RUNNING か *_PENDING のままなら止める
; （上の「PREINSTALL」(5)）。未登録・STOPPED なら素通り。自分で止めた（`flag_var` が
; "1"）のに 30 秒で止まり切らなかったときは、最大 ${BANTO_HUB_SERVICE_STOP_EXTRA_WAIT_SECONDS}
; 秒だけ PENDING が明けるのを待ち、STOPPED になれば続行する（フラグは "1" のまま。
; 再開は POSTINSTALL か中止の道）。それ以外（RUNNING のまま = 停止を拒否された、まだ
; PENDING、頼んでいない）は戻す操作が無いので `flag_var` を "0" にし、もう片方の止めた
; サービスを戻してから（RestoreStoppedBantoHubServices）案内し `Abort`。案内の文言は
; $1（"pending"＝自分で止めたが止まり切らない／"running"＝停止を拒まれた／"nowait"＝
; 頼んでいないのに動いている）で分け、"pending" は止まり切った後の `sc start` まで
; 案内する（もう一度実行しても再開しないため）。$0・$1 と
; $StopWaitCounter（共通部品の変数）・$ServiceRestoreNote・$ServiceRestoreFailed を壊す。
;   `action`: 文言に出す「インストール」／「アンインストール」。
!macro RequireBantoHubServiceInactive service_name flag_var action
  nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"RUNNING" >nul'
  Pop $0
  ${If} $0 != "0"
    nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"PENDING" >nul'
    Pop $0
  ${EndIf}
  ${If} $0 == "0"
    DetailPrint "banto-hub: ${service_name} サービスが止まっていません（RUNNING か PENDING のまま）。"
    StrCpy $1 "nowait"
    ${If} ${flag_var} == "1"
      DetailPrint "banto-hub: 停止の完了を最大 ${BANTO_HUB_SERVICE_STOP_EXTRA_WAIT_SECONDS} 秒待ちます..."
      StrCpy $StopWaitCounter 0
      ${Do}
        nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"PENDING" >nul'
        Pop $0
        ${If} $0 != "0"
          ${Break}
        ${EndIf}
        IntOp $StopWaitCounter $StopWaitCounter + 1
        ${If} $StopWaitCounter >= ${BANTO_HUB_SERVICE_STOP_EXTRA_WAIT_SECONDS}
          ${Break}
        ${EndIf}
        Sleep 1000
      ${Loop}
      ${If} $0 == "0"
        StrCpy $1 "pending"
      ${Else}
        nsExec::ExecToStack 'cmd /c sc query "${service_name}" | findstr /C:"STOPPED" >nul'
        Pop $0
        ${If} $0 == "0"
          StrCpy $1 "stopped"
        ${Else}
          StrCpy $1 "running"
        ${EndIf}
      ${EndIf}
    ${EndIf}
    ${If} $1 == "stopped"
      DetailPrint "banto-hub: ${service_name} サービスの停止が完了しました。続行します。"
    ${Else}
      StrCpy ${flag_var} "0"
      DetailPrint "banto-hub: ${service_name} サービスが止まり切らないため（$1）、${action}を中止します。ファイルはまだ変更していません。"
      !insertmacro RestoreStoppedBantoHubServices
      ; 文言は $1 で分ける（ChronoGazer の同名の確かめと同じ分け方）。"pending" はこの
      ; インストーラが止めたサービスなので、止まり切った後の開始まで案内する（フラグは
      ; "0" にしたので、もう一度実行しても再開しない）。
      ${If} $1 == "pending"
        MessageBox MB_OK|MB_ICONSTOP "${service_name} サービスはこのインストーラが止めましたが、まだ止まり切っていません（停止を頼んで、さらに ${BANTO_HUB_SERVICE_STOP_EXTRA_WAIT_SECONDS} 秒待ちましたが、PENDING のままです）。$\r$\n$\r$\n実行ファイルを入れ替えられないため、${action}を中止しました。ファイルはまだ変更していません。$ServiceRestoreNote$\r$\n$\r$\nsc query ${service_name} で STOPPED になってから、管理者権限の PowerShell から sc start ${service_name} で開始してください（${SERVICE_COMMON_OPS_DOC}）。このインストーラをもう一度実行しても、止まっているサービスは再開しません。" /SD IDOK
      ${ElseIf} $1 == "running"
        MessageBox MB_OK|MB_ICONSTOP "${service_name} サービスが停止の要求を受け付けず、RUNNING のままです。$\r$\n$\r$\n実行ファイルを入れ替えられないため、${action}を中止しました。ファイルはまだ変更していません。サービスは動いたままです。$ServiceRestoreNote$\r$\n$\r$\nサービスの状態を確かめ（sc query ${service_name}）、止めてからもう一度実行してください（${SERVICE_COMMON_OPS_DOC}）。" /SD IDOK
      ${Else}
        MessageBox MB_OK|MB_ICONSTOP "${service_name} サービスがまだ止まっていません（RUNNING か PENDING のままです。このインストーラは停止を頼んでいません）。$\r$\n$\r$\n実行ファイルを入れ替えられないため、${action}を中止しました。ファイルはまだ変更していません。$ServiceRestoreNote$\r$\n$\r$\nサービスの状態を確かめ（sc query ${service_name}）、止まってからもう一度実行してください（${SERVICE_COMMON_OPS_DOC}）。" /SD IDOK
      ${EndIf}
      Abort "${service_name} サービスが止まっていないため、${action}を中止しました。"
    ${EndIf}
  ${EndIf}
!macroend

; 止めたサービスの戻し（上の「中止の道での止めたサービスの戻し」。各中止の道が案内の
; MessageBox の前に呼び、`.onInstFailed`／`un.onUninstFailed` も呼ぶ）。止めてまだ
; 戻していないサービス（フラグが "1"）を Hub → Sink の順に `sc start` し RUNNING まで
; 待つ（共通部品 StartServiceIfStoppedAndWait が $0・$1・$StopWaitCounter を壊し、
; フラグを "0" に戻す）。共通部品がフラグを "0" に戻すので、2 回呼ばれても 2 回目は
; 何もしない。窓は出さない: 結果を $ServiceRestoreNote（案内に含める 1 文）と
; $ServiceRestoreFailed（再開できなかったサービス。無ければ ""）に返す。
; 呼び出し側が $1〜$3 に文言の材料を持っているので、$0〜$3 は専用の変数（RestoreSaved0〜3）に退避して保つ
; （共通部品の nsExec::ExecToStack が出力をスタックに残すので Push／Pop は使えない）。
!macro RestoreStoppedBantoHubServices
  StrCpy $RestoreSaved0 $0
  StrCpy $RestoreSaved1 $1
  StrCpy $RestoreSaved2 $2
  StrCpy $RestoreSaved3 $3
  ; $2 = 再開できなかったサービス、$3 = 再開を試みたサービス（どちらも「・」区切り）。
  StrCpy $2 ""
  StrCpy $3 ""
  ${If} $BantoHubWasRunning == "1"
    StrCpy $3 "BantoHub"
  ${EndIf}
  !insertmacro StartServiceIfStoppedAndWait "BantoHub" $BantoHubWasRunning
  ${If} $0 != "0"
    StrCpy $2 "BantoHub ($0)"
  ${EndIf}
  ${If} $SinkWasRunning == "1"
    ${If} $3 == ""
      StrCpy $3 "BantoHubSink"
    ${Else}
      StrCpy $3 "$3・BantoHubSink"
    ${EndIf}
  ${EndIf}
  !insertmacro StartServiceIfStoppedAndWait "BantoHubSink" $SinkWasRunning
  ${If} $0 != "0"
    ${If} $2 == ""
      StrCpy $2 "BantoHubSink ($0)"
    ${Else}
      StrCpy $2 "$2・BantoHubSink ($0)"
    ${EndIf}
  ${EndIf}
  StrCpy $ServiceRestoreFailed $2
  ${If} $3 == ""
    StrCpy $ServiceRestoreNote "再開するサービスはありません（その前に止めたサービスは無いか、戻さないと決めたものだけです）。"
  ${ElseIf} $2 == ""
    StrCpy $ServiceRestoreNote "その前に止めていたサービス（$3）は再開し、RUNNING になったことを確かめました。"
  ${Else}
    StrCpy $ServiceRestoreNote "その前に止めていたサービスのうち $2 は再開できませんでした。サービスの状態を確かめ（sc query BantoHub、sc query BantoHubSink）、管理者権限の PowerShell から sc start で開始してください（${SERVICE_COMMON_OPS_DOC}）。"
    DetailPrint "banto-hub: 止めていたサービスを再開できませんでした（$2）。管理者権限の PowerShell から sc start で開始してください（${SERVICE_COMMON_OPS_DOC}）。"
  ${EndIf}
  StrCpy $3 $RestoreSaved3
  StrCpy $2 $RestoreSaved2
  StrCpy $1 $RestoreSaved1
  StrCpy $0 $RestoreSaved0
!macroend

; P5: ファイルの上書きの結果の確かめ（上の「ファイルの上書きの結果」節。テンプレートの
; 当て直しが Section Install の `File` の直後に挿入する。直前の `ClearErrors` も
; 当て直しが置く）。エラーフラグが立っていれば、上書き前に止めたサービスを戻してから
; 案内して `Abort`。$ServiceRestoreNote・$ServiceRestoreFailed を壊す。
!macro RequireBantoHubFilesCopied
  ${If} ${Errors}
    DetailPrint "banto-hub: ファイルの上書きに失敗したものがあります。旧い実行ファイルと新しい実行ファイルが混ざらないよう、上書き前に止めたサービスを戻してからインストールを中止します。"
    !insertmacro RestoreStoppedBantoHubServices
    MessageBox MB_OK|MB_ICONSTOP "ファイルの上書きに失敗したものがあります（使用中か、書き込めませんでした）。$\r$\n$\r$\n一部のファイルだけ新しい状態なので、インストールを失敗として終えます（各ファイルは丸ごと旧いか丸ごと新しいかのどちらかで、壊れてはいません）。$ServiceRestoreNote$\r$\n$\r$\nBantoHub のデスクトップシェルと、コンソールで動かしている banto-hub.exe・banto-hub-sink.exe を終了してから、このインストーラをもう一度実行してください。" /SD IDOK
    Abort "ファイルの上書きに失敗したため、インストールを失敗として終えました。"
  ${EndIf}
!macroend

; 使用中の exe の確かめの 1 つ分（上の PREUNINSTALL の (a)）。あって読み書きで
; 開けなければ $1 に名前を足す。`FileOpen ... a` は無いファイルを作るので、
; 先に有無を見る。開けたら中身を変えずに閉じる。$0 を壊す。
!macro CheckBantoHubExeUnlocked exe_name
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

; P6 (a): 4 つの exe のどれかが使用中なら、止めたサービスを戻してから案内して `Abort`
; （まだ何も変更していない。上の PREUNINSTALL の (a)）。$0〜$2・$ServiceRestoreNote・
; $ServiceRestoreFailed を壊す。
!macro RequireBantoHubExesUnlocked
  StrCpy $1 ""
  !insertmacro CheckBantoHubExeUnlocked "banto-hub-shell.exe"
  !insertmacro CheckBantoHubExeUnlocked "banto-hub.exe"
  !insertmacro CheckBantoHubExeUnlocked "banto-hub-elev.exe"
  !insertmacro CheckBantoHubExeUnlocked "banto-hub-sink.exe"
  ${If} $1 != ""
    StrCpy $2 $1
    DetailPrint "banto-hub: 使用中の実行ファイルがあります（$2）。消せないため、止めたサービスを戻してからアンインストールを中止します（まだ何も変更していません）。"
    !insertmacro RestoreStoppedBantoHubServices
    MessageBox MB_OK|MB_ICONSTOP "使用中の実行ファイルがあります（$2）。$\r$\n$\r$\n消せないため、アンインストールを中止しました。まだ何も変更していません。$ServiceRestoreNote$\r$\n$\r$\nBantoHub のデスクトップシェルと、コンソールで動かしている banto-hub.exe・banto-hub-sink.exe を終了してから、もう一度アンインストールしてください。" /SD IDOK
    Abort "使用中の実行ファイルがあるため、アンインストールを中止しました。"
  ${EndIf}
!macroend

; 消えたことの確かめの 1 つ分（上の PREUNINSTALL の (d)）。残っていれば $2 に名前を足す。
!macro CheckBantoHubExeDeleted exe_name
  ${If} ${FileExists} "$INSTDIR\${exe_name}"
    StrCpy $2 "$2 ${exe_name}"
  ${EndIf}
!macroend

; 解除した BantoHub サービスの登録の戻し（P6 (d) で exe が消し残ったとき。(b) で Hub の
; 解除に失敗したときは Hub の登録は残っているので使わない）。(b) で解除していなければ
; （$BantoHubUnregistered が "1" でない = もともと未登録）戻すものは無い。
; `banto-hub-elev.exe` と `banto-hub.exe` が残っていれば `service-install` で戻す
; （起動種別は手動に戻る）。結果を $3 に文で足し、戻せなければ $BantoHubWasRunning を
; "0" にして再開させない（RestoreStoppedBantoHubServices が飛ばす）。$0・$3 を壊す。
!macro RestoreBantoHubServiceRegistration
  ; ExecWait は起動できないとエラーフラグを立てるだけで $0 を変えないので、フラグも見る。
  ${If} $BantoHubUnregistered != "1"
    StrCpy $BantoHubWasRunning "0"
    StrCpy $3 "$3 BantoHub: もともと登録が無かったので戻すものはありません。"
  ${ElseIf} ${FileExists} "$INSTDIR\banto-hub-elev.exe"
  ${AndIf} ${FileExists} "$INSTDIR\banto-hub.exe"
    DetailPrint "banto-hub: 解除した BantoHub サービスの登録を banto-hub-elev.exe service-install で戻します..."
    ClearErrors
    ExecWait '"$INSTDIR\banto-hub-elev.exe" service-install' $0
    ${If} ${Errors}
      StrCpy $0 "exec-error"
    ${EndIf}
    ${If} $0 == 0
      StrCpy $3 "$3 BantoHub: 戻しました。"
    ${Else}
      StrCpy $BantoHubWasRunning "0"
      StrCpy $3 "$3 BantoHub: 戻せませんでした（service-install の終了コード $0）。"
    ${EndIf}
  ${Else}
    StrCpy $BantoHubWasRunning "0"
    StrCpy $3 "$3 BantoHub: banto-hub-elev.exe か banto-hub.exe が消えているため戻せません。"
  ${EndIf}
!macroend

; 解除した BantoHubSink サービスの登録の戻し（P6 (b) で Sink の解除が通って Hub の解除に
; 失敗したときと、(d) で exe が消し残ったとき）。(b) で解除していなければ
; （$SinkUnregistered が "1" でない = もともと未登録）戻すものは無い。
; `banto-hub-sink.exe` が残っていれば `install` と `grant-service-acl` で戻す（起動種別は
; 手動に戻る）。結果を $3 に文で足し、戻せなければ $SinkWasRunning を "0" にして
; 再開させない（ACL だけ付け直せなかったときは登録は戻っているので
; 再開する）。$0・$3・$4 を壊す。
!macro RestoreBantoHubSinkServiceRegistration
  ${If} $SinkUnregistered != "1"
    StrCpy $SinkWasRunning "0"
    StrCpy $3 "$3 BantoHubSink: もともと登録が無かったので戻すものはありません。"
  ${ElseIf} ${FileExists} "$INSTDIR\banto-hub-sink.exe"
    DetailPrint "banto-hub: 解除した BantoHubSink サービスの登録を banto-hub-sink.exe install で戻します..."
    ClearErrors
    ExecWait '"$INSTDIR\banto-hub-sink.exe" install' $0
    ${If} ${Errors}
      StrCpy $0 "exec-error"
    ${EndIf}
    ${If} $0 == 0
      ClearErrors
      ExecWait '"$INSTDIR\banto-hub-sink.exe" grant-service-acl' $4
      ${If} ${Errors}
        StrCpy $4 "exec-error"
      ${EndIf}
      ${If} $4 == 0
        StrCpy $3 "$3 BantoHubSink: 戻しました。"
      ${Else}
        StrCpy $3 "$3 BantoHubSink: 戻しましたが Operators のサービス ACL は付け直せませんでした（grant-service-acl の終了コード $4）。"
      ${EndIf}
    ${Else}
      StrCpy $SinkWasRunning "0"
      StrCpy $3 "$3 BantoHubSink: 戻せませんでした（install の終了コード $0）。"
    ${EndIf}
  ${Else}
    StrCpy $SinkWasRunning "0"
    StrCpy $3 "$3 BantoHubSink: banto-hub-sink.exe が消えているため戻せません。"
  ${EndIf}
!macroend

; P6 (d): テンプレートの `Delete` の後に exe が残っていれば、`uninstall.exe` と
; 「アプリ」の登録を消す前に止める（上の PREUNINSTALL の (d)）。サービスの登録は
; もう解除されているので、残っている exe で頼める分だけ戻し（結果は $3 に文で集める）、
; 戻せなかったサービスはフラグを "0" にして再開させず、戻せた分は案内の前に再開する。
; $0〜$4・$ServiceRestoreNote・$ServiceRestoreFailed を壊す。
!macro RequireBantoHubExesDeleted
  StrCpy $2 ""
  !insertmacro CheckBantoHubExeDeleted "banto-hub-shell.exe"
  !insertmacro CheckBantoHubExeDeleted "banto-hub.exe"
  !insertmacro CheckBantoHubExeDeleted "banto-hub-elev.exe"
  !insertmacro CheckBantoHubExeDeleted "banto-hub-sink.exe"
  ${If} $2 != ""
    DetailPrint "banto-hub: 実行ファイルを消せませんでした（$2）。uninstall.exe と「アプリ」の登録は残したまま、アンインストールを中止します。"
    StrCpy $3 ""
    !insertmacro RestoreBantoHubServiceRegistration
    !insertmacro RestoreBantoHubSinkServiceRegistration
    DetailPrint "banto-hub: サービスの登録の戻し:$3 戻した登録の起動種別は手動です。戻せなかったものは、このインストーラをもう一度実行して入れ直すと戻ります。"
    !insertmacro RestoreStoppedBantoHubServices
    MessageBox MB_OK|MB_ICONSTOP "実行ファイルを消せませんでした（$2。使用中か、消せませんでした）。$\r$\n$\r$\nアンインストールを中止しました。uninstall.exe と「アプリ」の登録は残しています。解除していたサービスの登録:$3$\r$\n戻した登録の起動種別は手動です（自動開始にしていたなら付け直してください）。$ServiceRestoreNote$\r$\n$\r$\nBantoHub のデスクトップシェルと、コンソールで動かしている banto-hub.exe・banto-hub-sink.exe を終了してから、もう一度アンインストールするか、このインストーラをもう一度実行して入れ直してください（サービスの登録も戻ります）。" /SD IDOK
    Abort "実行ファイルを消せなかったため、アンインストールを中止しました。"
  ${EndIf}
!macroend

; --- フック本体 ---------------------------------------------------------

; 事前の確かめ（上の「事前の確かめ」節）。テンプレートの当て直し P3 が Section
; WebView2 の先頭に挿入する。
!macro BantoHubInstallPreflight
  ; (0) インストール先の固定。通れば $INSTDIR が畳んだ形に揃う（テンプレートの
  ; SetOutPath はこの後）。
  !insertmacro RequireDefaultInstallDir
  ; (1) 所有者・ACL の既定への戻し。インストール先がまだ無ければ（新規）飛ばす。
  ${If} ${FileExists} "$INSTDIR\*.*"
    !insertmacro ResetInstallDirAcl
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; (0)(1) は BantoHubInstallPreflight（Section WebView2 の先頭）で済んでいる。
  ; (2)〜(6)
  !insertmacro FixupHubOnlyInstallerMainBinaryName
  !insertmacro StopServiceIfRunning "BantoHubSink" $SinkWasRunning
  !insertmacro StopServiceIfRunning "BantoHub" $BantoHubWasRunning
  !insertmacro RequireBantoHubServiceInactive "BantoHubSink" $SinkWasRunning "インストール"
  !insertmacro RequireBantoHubServiceInactive "BantoHub" $BantoHubWasRunning "インストール"
  !insertmacro TerminateProcessIfRunning "banto-hub-shell.exe"
!macroend

; `.onInstFailed`（テンプレートの当て直し P4 が挿入する）。フックの中止の道はもう
; 戻し終えている（フラグが "0"）ので、ここで戻るのは `File` の窓の [中止]・
; `CheckIfAppIsRunning` の [キャンセル] の分だけ。他に窓が無いので、戻せなかった
; ときだけここで案内する。
!macro BantoHubOnInstFailed
  !insertmacro RestoreStoppedBantoHubServices
  ${If} $ServiceRestoreFailed != ""
    MessageBox MB_OK|MB_ICONEXCLAMATION "インストールを中止しましたが、$ServiceRestoreNote" /SD IDOK
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "banto-hub: Windows サービス (BantoHub) を登録しています..."
  ExecWait '"$INSTDIR\banto-hub-elev.exe" service-install' $0
  ${If} $0 != 0
    DetailPrint "banto-hub: BantoHub サービス登録に失敗しました（終了コード $0）。管理者権限の PowerShell から手動で次を実行してください: $\"$INSTDIR\banto-hub-elev.exe$\" service-install （docs/banto-hub-operations.md §12 参照） インストール先が Program Files の外（既定は C:\Program Files\BantoHub）だと、権限の昇格を防ぐためサービスは登録できません。その場合は既定の場所に入れ直してください。"
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

  ; (4) #630: サイドカーの設定ファイルの置き場を elev が確かめてから整え、
  ; 雛形を差し替える（モジュール doc「POSTINSTALL」節参照）。NSIS では
  ; 作らない（継承の DACL で作ってしまうため）。
  DetailPrint "banto-hub: サイドカーの設定ファイルの置き場 (%ProgramData%\BantoHub\sink) を整えています..."
  ExecWait '"$INSTDIR\banto-hub-elev.exe" prepare-sink-config' $0
  ${If} $0 != 0
    DetailPrint "banto-hub: prepare-sink-config に失敗しました（終了コード $0）。管理者権限の PowerShell から手動で次を実行してください: $\"$INSTDIR\banto-hub-elev.exe$\" prepare-sink-config （docs/banto-hub-operations.md §12「サイドカーの初回セットアップ」参照）"
  ${Else}
    DetailPrint "banto-hub: サイドカーの設定ファイルの置き場を整え、雛形 (banto-hub-sink.toml.example) を置きました。管理者のメモ帳で雛形をコピーして banto-hub-sink.toml を作り、編集の後に prepare-sink-config をもう一度実行してください。"
  ${EndIf}

  ; #630（#655 のレビュー Should 2）: 旧い置き場（ルート直下・exe の隣）に設定ファイルが
  ; 残っていれば、この版の sink は読まずに起動しない。上書きインストールで (5) が sink を
  ; 再開しても、すぐ止まるので、インストーラの詳細に移し方を残す。パスは (4) の旧い処理と
  ; 同じ求め方（PerMachine の $APPDATA = %ProgramData%）と $INSTDIR。旧いファイルには触らない。
  ${If} ${FileExists} "$APPDATA\BantoHub\banto-hub-sink.toml"
    DetailPrint "banto-hub: 旧い置き場に設定ファイル ($APPDATA\BantoHub\banto-hub-sink.toml) があります。この版の DB Sink サイドカーは旧い置き場を読まず、残っている間は起動しません。移し方（新しい置き場で作り直し・キーの失効と再発行・旧いファイルの削除）は docs/banto-hub-operations.md §12 を参照してください。上書き前に稼働していた場合も BantoHubSink は再開を試みますが、設定を移すまで止まります。"
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\banto-hub-sink.toml"
    DetailPrint "banto-hub: 旧い置き場に設定ファイル ($INSTDIR\banto-hub-sink.toml) があります。この版の DB Sink サイドカーは旧い置き場を読まず、残っている間は起動しません。移し方は docs/banto-hub-operations.md §12 を参照してください。上書き前に稼働していた場合も BantoHubSink は再開を試みますが、設定を移すまで止まります。"
  ${EndIf}

  ; §7 決定4: PREINSTALL で止めたものだけ再開する（Hub → Sink の順）。
  ; 新規インストールでは両変数とも未設定のため、このブロックは実行されない。
  !insertmacro StartServiceIfStopped "BantoHub" $BantoHubWasRunning
  !insertmacro StartServiceIfStopped "BantoHubSink" $SinkWasRunning
  ; (6) silent / passive でテンプレートが作ったデスクトップショートカットを削除
  ${If} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
    Delete "$DESKTOP\${PRODUCTNAME}.lnk"
    DetailPrint "banto-hub: デスクトップショートカットを削除しました（スタートメニューのみ作成、設計 §7-3）"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; ここではサービスの登録を解除しない。解除は Section Uninstall の CheckIfAppIsRunning
  ; と使用中の exe の確かめ（P6 (a)）の後、ファイルを消す直前の
  ; UnregisterBantoHubServices で行う（上の「PREUNINSTALL と Section Uninstall の順」）。
  !insertmacro TerminateProcessIfRunning "banto-hub-shell.exe"
  !insertmacro StopServiceIfRunning "BantoHubSink" $SinkWasRunning
  !insertmacro StopServiceIfRunning "BantoHub" $BantoHubWasRunning
  !insertmacro RequireBantoHubServiceInactive "BantoHubSink" $SinkWasRunning "アンインストール"
  !insertmacro RequireBantoHubServiceInactive "BantoHub" $BantoHubWasRunning "アンインストール"
!macroend

; サービス登録の解除（上の (b)。テンプレートの当て直し P6 が Section Uninstall の
; 使用中の exe の確かめの直後・`Delete` の前に挿入する）。ChronoGazer の
; UnregisterChronoGazerService にそろえ、**解除に失敗したらファイルを消さずに案内して
; `Abort`** する（登録が残ったまま ImagePath の exe が消えるのを防ぐ。#977 の指摘 P2）。
; どちらの失敗の道も、止めたサービス（登録がある分）を戻してから案内する。
;
; 順は Sink → Hub（サイドカーは Hub に接続する側なので、止める順と同じく先に片付ける。
; 解除の順に依存は無い）。Sink の解除が通って Hub の解除が失敗したときは、Sink の登録が
; もう無いので再開できない。戻しの前に Sink の登録を
; RestoreBantoHubSinkServiceRegistration（(d) と同じ `install`＋`grant-service-acl`）で
; 戻し、戻せなければ $SinkWasRunning を "0" にして案内する。
;
; 各サービスは先に登録の有無（ImagePath）を見る。未登録なら解除は要らず飛ばす
; （インストールの登録が失敗した・利用者が先に解除した。`uninstall`／`service-uninstall`
; は未登録を終了コード 1 で返すので、先に確かめないとアンインストールできなくなる）。
; 登録が残っているのに頼む exe が無いとき（elev も banto-hub.exe も無い、または
; banto-hub-sink.exe が無い）は `sc delete` で解除する: exe はもう無いので「ImagePath の
; exe が消える」心配は無いが、登録を残して続行すると、インストール先ごと消えた後に
; 壊れた登録だけが残り、次のインストールの `service-install`（登録済みは作り直さない）が
; 旧いパスのまま通ってしまう。解除できなければ他と同じく `Abort`。
; 解除が 0 で返っても登録が消えたとは限らない（下の RequireBantoHubServiceDeleteNotPending）。
; $0〜$4・$BantoHubUnregistered・$SinkUnregistered・$ServiceRestoreNote・
; $ServiceRestoreFailed を壊す。

; 削除の保留の確かめ（UnregisterBantoHubServices の各解除が 0 で返った直後。未登録で
; 飛ばしたときは呼ばない）。DeleteService は他のプロセス（サービスの管理画面・
; `sc query` を開いたままのツールなど）がそのサービスのハンドルを持っていると、登録に
; 「削除の印」を付けて成功を返す。印付きの登録は `HKLM\SYSTEM\CurrentControlSet\
; Services\<名前>` に `DeleteFlag`=1 で残り、起動できず（1072）、ハンドルが全部閉じた
; ときに消える。終了コードだけを見てファイルを消すと、登録が残ったまま ImagePath の
; exe が消え、再インストールの `service-install` が印付きを「登録済み」として通して
; 後からサービスだけ消える。そこで `DeleteFlag` を `ReadRegDWORD` で読む（キーごと
; 消えていればエラーフラグが立つ = 消えた。値が 1 なら保留。ImagePath の有無では、
; 消える前の一瞬の残りと保留を区別できないので DeleteFlag を使う）。
; 保留なら**登録の戻しも再開も試みない**（戻しの `install`／`service-install` は
; 「登録済み」で素通りし、`sc start` は 1072 で失敗する）: そのサービスの `flag_var` を
; "0" にし、`restore_sink` が 1（Hub の解除の後）なら先に解除した Sink の登録を
; RestoreBantoHubSinkServiceRegistration で戻し（結果は $3。Sink が保留のときは Hub を
; まだ解除していないので戻すものは無く、Hub だけ再開する）、止めたサービスを戻して
; から案内して `Abort`。もう一度アンインストールすると、消えた登録は未登録として
; 飛ばすので先へ進める。$3・$4・$ServiceRestoreNote・$ServiceRestoreFailed を壊す
; （`restore_sink` が 1 のときは $0 も）。
!macro RequireBantoHubServiceDeleteNotPending service_name flag_var restore_sink
  ClearErrors
  ReadRegDWORD $4 HKLM "SYSTEM\CurrentControlSet\Services\${service_name}" "DeleteFlag"
  ${If} ${Errors}
    ; キーごと消えた（または DeleteFlag が無い）= 保留ではない。
    ClearErrors
  ${ElseIf} $4 == 1
    StrCpy ${flag_var} "0"
    DetailPrint "banto-hub: ${service_name} の登録の削除が保留中です（別のプロセスがサービスを開いたままです）。登録の戻しと再開はせず、ファイルは消さずにアンインストールを中止します。"
    StrCpy $3 ""
    !if "${restore_sink}" == "1"
      ; 先に解除した Sink の登録を戻す（戻せなければ $SinkWasRunning が "0" になり再開しない）。
      !insertmacro RestoreBantoHubSinkServiceRegistration
      DetailPrint "banto-hub: 先に解除した BantoHubSink の登録の戻し:$3 戻した登録の起動種別は手動です。"
      StrCpy $3 "先に解除していた BantoHubSink の登録:$3$\r$\n戻した登録の起動種別は手動です（自動開始にしていたなら付け直してください）。"
    !endif
    !insertmacro RestoreStoppedBantoHubServices
    MessageBox MB_OK|MB_ICONSTOP "${service_name} サービスの登録の削除が保留中です（サービスの管理画面などの別のツールがサービスを開いたままです）。$\r$\n$\r$\nファイルは消さずにアンインストールを中止しました。保留中の登録は戻しも再開もしていません。$3$ServiceRestoreNote$\r$\n$\r$\n開いているツールを閉じるか、Windows を再起動してから、もう一度アンインストールしてください（もう一度実行すると、消えた登録は飛ばします）。" /SD IDOK
    Abort "${service_name} サービスの登録の削除が保留中のため、アンインストールを中止しました。"
  ${EndIf}
!macroend

!macro UnregisterBantoHubServices
  StrCpy $BantoHubUnregistered ""
  StrCpy $SinkUnregistered ""

  DetailPrint "banto-hub: DB Sink サイドカーサービス (BantoHubSink) の登録を解除しています..."
  ReadRegStr $1 HKLM "SYSTEM\CurrentControlSet\Services\BantoHubSink" "ImagePath"
  ; ExecWait は起動できないとエラーフラグを立てるだけで $0 を変えないので、フラグも見る。
  ClearErrors
  ${If} $1 == ""
    DetailPrint "banto-hub: サービス BantoHubSink は登録されていないので、登録の解除は飛ばします。"
    StrCpy $0 0
  ${ElseIf} ${FileExists} "$INSTDIR\banto-hub-sink.exe"
    ExecWait '"$INSTDIR\banto-hub-sink.exe" uninstall' $0
  ${Else}
    DetailPrint "banto-hub: banto-hub-sink.exe が見つからないため、sc delete BantoHubSink で登録を解除します..."
    nsExec::ExecToLog 'sc.exe delete BantoHubSink'
    Pop $0
  ${EndIf}
  ${If} ${Errors}
    StrCpy $0 "exec-error"
  ${EndIf}
  ${If} $0 != 0
    StrCpy $2 $0
    DetailPrint "banto-hub: BantoHubSink の登録解除に失敗しました（終了コード $2）。ファイルは消さずに、止めたサービスを戻してからアンインストールを中止します（まだ何も変更していません）。${SERVICE_COMMON_OPS_DOC} の手順で手動対応してください。"
    !insertmacro RestoreStoppedBantoHubServices
    MessageBox MB_OK|MB_ICONSTOP "BantoHubSink サービスの登録解除に失敗しました（終了コード $2）。$\r$\n$\r$\nファイルは消さずにアンインストールを中止しました。まだ何も変更していません。$ServiceRestoreNote$\r$\n$\r$\n${SERVICE_COMMON_OPS_DOC} の手順で対処してから（手動で解除するなら管理者権限の PowerShell から $\"$INSTDIR\banto-hub-sink.exe$\" uninstall）、もう一度アンインストールしてください。" /SD IDOK
    Abort "BantoHubSink サービスの登録解除に失敗したため、アンインストールを中止しました。"
  ${EndIf}
  ${If} $1 != ""
    ; 0 で返っても削除の印だけ付いて登録が残ることがある。保留なら Hub はまだ解除して
    ; いないので、戻す登録は無く Hub だけ再開して止める。
    !insertmacro RequireBantoHubServiceDeleteNotPending "BantoHubSink" $SinkWasRunning 0
    StrCpy $SinkUnregistered "1"
  ${EndIf}

  DetailPrint "banto-hub: Windows サービス (BantoHub) の登録を解除しています..."
  ReadRegStr $1 HKLM "SYSTEM\CurrentControlSet\Services\BantoHub" "ImagePath"
  ClearErrors
  ${If} $1 == ""
    DetailPrint "banto-hub: サービス BantoHub は登録されていないので、登録の解除は飛ばします。"
    StrCpy $0 0
  ${ElseIf} ${FileExists} "$INSTDIR\banto-hub-elev.exe"
    ExecWait '"$INSTDIR\banto-hub-elev.exe" service-uninstall' $0
  ${ElseIf} ${FileExists} "$INSTDIR\banto-hub.exe"
    ; 壊れたインストール（elev が無い）に備えたフォールバック（同等の処理、
    ; apps/banto-hub/host/src/bin/banto_hub/win_service.rs 参照）。
    DetailPrint "banto-hub: banto-hub-elev.exe が見つからないため banto-hub.exe uninstall にフォールバックします"
    ExecWait '"$INSTDIR\banto-hub.exe" uninstall' $0
  ${Else}
    DetailPrint "banto-hub: banto-hub-elev.exe も banto-hub.exe も見つからないため、sc delete BantoHub で登録を解除します..."
    nsExec::ExecToLog 'sc.exe delete BantoHub'
    Pop $0
  ${EndIf}
  ${If} ${Errors}
    StrCpy $0 "exec-error"
  ${EndIf}
  ${If} $0 != 0
    StrCpy $2 $0
    DetailPrint "banto-hub: BantoHub の登録解除に失敗しました（終了コード $2）。ファイルは消さずにアンインストールを中止します。${SERVICE_COMMON_OPS_DOC} の手順で手動対応してください。"
    ; 先に解除した Sink の登録を戻してから、止めたサービスを再開し、案内して止める
    ; （Sink の登録を戻せなければ $SinkWasRunning が "0" になり、Hub だけ再開する）。
    ; 登録の戻しの結果は $3。
    StrCpy $3 ""
    !insertmacro RestoreBantoHubSinkServiceRegistration
    DetailPrint "banto-hub: 先に解除した BantoHubSink の登録の戻し:$3 戻した登録の起動種別は手動です。"
    !insertmacro RestoreStoppedBantoHubServices
    MessageBox MB_OK|MB_ICONSTOP "BantoHub サービスの登録解除に失敗しました（終了コード $2）。$\r$\n$\r$\nファイルは消さずにアンインストールを中止しました。先に解除していた BantoHubSink の登録:$3$\r$\n戻した登録の起動種別は手動です（自動開始にしていたなら付け直してください）。$ServiceRestoreNote$\r$\n$\r$\n${SERVICE_COMMON_OPS_DOC} の手順で対処してから、もう一度アンインストールしてください。" /SD IDOK
    Abort "BantoHub サービスの登録解除に失敗したため、アンインストールを中止しました。"
  ${EndIf}
  ${If} $1 != ""
    ; 保留なら、先に解除した Sink の登録を戻してから（Sink が消えていれば）止める。
    !insertmacro RequireBantoHubServiceDeleteNotPending "BantoHub" $BantoHubWasRunning 1
    StrCpy $BantoHubUnregistered "1"
  ${EndIf}

  ; %ProgramData%\BantoHub\（DB・サイドカー設定・サービスログ）は
  ; 意図的に削除しない（§4.5・§7 決定7 - 再インストールで設定・データが
  ; 戻ることを優先する）。完全削除は docs の手順に従い利用者が手動で行う。
!macroend

; `un.onUninstFailed`（テンプレートの当て直し P4 が挿入する）。`.onInstFailed` と同じ。
!macro BantoHubOnUninstFailed
  !insertmacro RestoreStoppedBantoHubServices
  ${If} $ServiceRestoreFailed != ""
    MessageBox MB_OK|MB_ICONEXCLAMATION "アンインストールを中止しましたが、$ServiceRestoreNote" /SD IDOK
  ${EndIf}
!macroend
