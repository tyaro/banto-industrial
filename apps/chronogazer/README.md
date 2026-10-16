# ChronoGazer

デジタル記録計（ペーパーレスレコーダのソフトウェア版）。PLC通信 + タグ
データ保存 + リアルタイム/ヒストリカル/ハイブリッドトレンド + 計器表示を
1台に統合する、banto-industrial 最初の製品アプリ（docs/plan.md §4）。
要件定義は
[../../docs/recorder-requirements.md](../../docs/recorder-requirements.md)。
banto テンプレート（Tauri + SvelteKit）由来。SvelteKit 3・TypeScript 6（banto v4.0.0 の admin-template に
追従、2026-10-05〜）。アプリ内の import は `#lib/...`（`package.json` の `imports`。`.ts` は `.js` を付けて
書く、`.svelte` はそのまま）で、SvelteKit の設定は `vite.config.ts` の `sveltekit({...})` にある
（`svelte.config.js` は無い）。ナビ・設定カテゴリの表の `path` は `AppPath`（存在しないルートは型エラー）、
URL にするときは `resolveAppPath()`（`src/lib/navigation.ts`）を通す。

## 現状（R1-A 段階）

**アプリ骨格のみ実装済み**で、記録計としての中核機能（収集・トレンド表示）
はまだ動かない。

- 動くもの: ログイン、設定画面（テーマ・認証・バックアップ等、banto
  テンプレート由来の機能）、LANモード（`banto-serve`）/ Tauri デスクトップ
  の両起動経路
- 動かないもの: 監視・ヒストリカル・イベントの各画面は実データ無しの
  プレースホルダ表示（`src/routes/(app)/monitor`・`historical`・`events`
  それぞれの `+page.svelte` に「R1-A: プレースホルダ」と明記）。I系クレート
  （banto-tags/banto-plc/banto-tstore/banto-collect/banto-tsquery）は
  `core/Cargo.toml` に依存としては追加済みだが、**まだ REST/Tauri
  コマンドのどこにも配線されていない**（同ファイルの依存コメント参照）。
  収集ランタイム統合は R1-C、実データでの監視画面は R1-D で入る予定

今後の実施計画（R1-B 設定CRUD → R1-C 収集ランタイム統合 → R1-D 監視画面）は
[../../docs/r1-plan.md](../../docs/r1-plan.md) を参照。

## 起動方法

デスクトップ（Tauri）:

```sh
pnpm --filter chronogazer tauri dev
```

**開発では `CHRONOGAZER_ROOT` を作業用のフォルダに向ける**（#392 B2-4b、2026-10-14〜）。デスクトップ版も
既定では `%ProgramData%\ChronoGazer\` を使うので、何も指定しないと `tauri dev` が本番と同じ場所に DB を作る
（下の「データの置き場所」）。絶対パスで指定する（例 PowerShell: `$env:CHRONOGAZER_ROOT = "D:\scratch\cg-dev"`、
Git Bash: `CHRONOGAZER_ROOT=/d/scratch/cg-dev pnpm --filter chronogazer tauri dev`）。

### データの置き場所（デスクトップ版、#392 B2-4b、2026-10-14〜）

デスクトップ版も `chronogazer-server`（下）と同じ**データのルート**を使う: 既定 `%ProgramData%\ChronoGazer\`、
環境変数 `CHRONOGAZER_ROOT`（**絶対パスだけ**。`.`・`..` を含むパスも断る）で変えられる。サービスと
デスクトップで DB が分かれると、同じ PLC を二重に読み、履歴も割れるため。

- ルートの下: DB `chronogazer.sqlite3`、時系列データ `data\`（`data.dir` の既定）、バックアップ
  `backups\chronogazer.sqlite3\`、監査の保留 `audit-spool\chronogazer.sqlite3\`、Hub の API キー
  `secrets\hub-keys\`（DPAPI で守ったファイル。以前の版は OS キーリング）。デスクトップ版はログ `logs\` と
  `run\endpoint.json` は書かない（`chronogazer-server` のもの）。
- ルートが無ければ作る。そのときは継承しない ACL（SYSTEM・Administrators にフル、作った利用者に「変更」、
  Users なし）を付ける。**同じ PC の他の利用者はそのデータを使えない**（起動するとデータを使えない旨のエラーの窓が出る）。
  既にあるルートの ACL には触らない。ルートを使えない・作れないときは理由と「終了」だけの窓が出る。
- 同じルートを `chronogazer-server`（コンソール・サービス）が使っていると、デスクトップ版は起動せず
  「この PC で ChronoGazer のサービス（またはコンソール版）が同じデータを使っています」の窓を出す。
- 利用者ごとに残るもの: 自動ログインのパスワード（OS キーリング）、画面の状態（WebView のデータ、
  `%LOCALAPPDATA%\dev.tyaro.chronogazer\`）、トレイの初回の通知の既読の印
  （`%APPDATA%\dev.tyaro.chronogazer\tray-hint-shown`）、旧データの案内の既読の印
  （`%LOCALAPPDATA%\dev.tyaro.chronogazer\legacy-data-notice-shown`）。
- **以前の版のデータ**（`%APPDATA%\dev.tyaro.chronogazer\` の DB・時系列データ・バックアップ）は
  **使われない**。読まない・移さない・消さない。旧 DB が残っていれば、起動に成功した後に 1 回だけ
  （利用者ごと）「使われません。要らなければ手で削除してください」と案内する。アカウント・接続・タグ・
  設定は引き継がれないので初回セットアップからやり直し、Hub 経由のタグを使うときは「設定 > Hub接続」から
  接続し直す（以前の版が OS キーリングに入れた Hub のキーも残るだけで使われない）。

LANモード（Tauri無し、REST + 静的配信のみのバイナリ。webkit2gtk が無い
コンテナ環境でも動く開発用サーバー。設定画面の LAN モードトグルが将来
呼び出すのと同じ経路のプレビュー用途）:

```sh
cargo run -p chronogazer-core --bin banto-serve
# 実フロントエンドを埋め込む場合は先に `pnpm --filter chronogazer build`
# を実行してから `--features embed-ui` を付けて起動する
```

`banto-serve` の既定は `PORT=8721` / `BANTO_BIND=0.0.0.0`（LAN 越しの
動作確認をしやすくするための開発用デフォルト。デスクトップ版 Tauri アプリ
の LAN モード既定は `127.0.0.1`）。`BANTO_DB`（既定
`./banto-dev.sqlite3`）・`BANTO_ALLOW_SETUP=1`（初回セットアップの許可）・
`BANTO_VIEWER_PUBLIC=1`（閲覧公開を ON で保存する。外しても OFF には戻らない）も
環境変数で指定できる（`core/src/bin/banto-serve.rs`）。

### 画面の無い本番用のエンジン `chronogazer-server`（2026-10-13〜、#392 B1-5）

`banto-serve`（開発・E2E 用）とは別の、本番用の headless host。コンソールで動かす形と、同じ exe を
Windows サービス `ChronoGazer` として動かす形（2026-10-15〜、#392 B3-2b。下の「Windows サービスとして
動かす」）がある。

```sh
cargo build -p chronogazer-core --bin chronogazer-server --features embed-ui
target\debug\chronogazer-server.exe        # Ctrl-C で停止
```

- **データ**: `%ProgramData%\ChronoGazer\`（無ければ作る）。環境変数 `CHRONOGAZER_ROOT` で変えられる
  （**絶対パスだけ**。相対パスは拒否して終了コード 3）。2026-10-14〜 デスクトップ版も同じルートを使う
  （上の「データの置き場所」。同時には動かせない）。以前の版のデスクトップ版の `%APPDATA%\dev.tyaro.chronogazer\`
  は読まない・移さない。配置は DB `chronogazer.sqlite3`、時系列データ `data\`、バックアップ `backups\`、
  Hub の API キー `secrets\hub-keys\`（DPAPI で守ったファイル。ディレクトリは SYSTEM・Administrators と
  起動した利用者だけ）、ログ `logs\`、所在 `run\endpoint.json`（動いている間だけ）。
- **待ち受け**: `http://127.0.0.1:<port>/`（設定 `server.port`、既定 8721）は常に開く。LAN は設定
  `server.enabled` がデスクトップ版と同じ条件を満たすときだけ `server.bind` に足す（`0.0.0.0` なら 1 本）。
  最初の管理者の作成（`/api/auth/setup`）は要求ごとに決まる: 要求の送り元と `Host` の両方がこの PC
  （`127.0.0.1`・`localhost`・`[::1]`）のときだけ受け付け、LAN からの要求では閉じる（`0.0.0.0` で 1 本に
  まとめていても同じ。外のページからの DNS rebinding も防ぐ。#392 B2-1、2026-10-14）。この PC のブラウザで
  `http://127.0.0.1:<port>/` を開けば作れる。
- **この PC からの接続（ログインなし、#392 B2-1）**: 要求の送り元と `Host` の両方がこの PC のときだけ、`POST /api/auth/grant/localDesktop` でログインせずにセッションを受け取れる。最初の
  管理者を作るまでの間は管理者、作った後は設定 `auth.local_grant`（`off`・`viewer`・`editor`・`admin`、
  **既定 `off`**）の権限。受け取った記録は監査ログに `login`（`via: localGrant`、操作者 `localDesktop`）で残る。
  設定は管理者がこの PC から `PUT /api/settings/auth`（`{"localGrant":"viewer"}` など）で変える（LAN からは
  変えられない）。発行をやめる方向は
  すぐ効き（発行済みのセッションも切れる）、新しい権限で入れるのは**次の起動から**。LAN からの要求
  （`0.0.0.0` でまとめた待ち受けでも）・LAN 専用の待ち受けでは発行しない。同じ PC で reverse proxy の後ろに置くと、この確認は効かない。
- **LAN の公開設定（#392 B2-1）**: 管理者が `GET`/`PUT /api/settings/server`（`{enabled,bind,port,viewerPublic}`）
  で読み書きできる。`PUT` はこの PC からの要求だけ受け付け、LAN の待ち受けをその場で開け閉めする。ポートの
  変更と `0.0.0.0` への出し入れは保存だけして `restartRequired: true` を返す（次の起動から）。ログイン不要
  モードはデスクトップ版の画面の機能なので、REST では読むだけで変えられない。
- **ログ**: `logs\chronogazer-server-YYYY-MM-DD.log`（各行に時刻。標準エラー出力もここへ向け直し、
  コンソールにも同じものを出す）。保持は設定 `log.retention_days`（既定 30 日、`0` で無制限）。
- **止め方**: Ctrl-C（後始末は最大 15 秒）。コンソールのウィンドウを閉じたときも、通常は最終の書き出しと
  `collection_stopped` を残してから終わる（OS の猶予に合わせて最大 4 秒。例外は下の既知の制限）。ログオフでも終わる
  （サインアウトをまたいで動かすのは下の Windows サービス）。
- **終了コード**: 0 正常、1 後始末が時間内に終わらなかった・最終の書き出しに失敗、2 同じデータを
  別の ChronoGazer（デスクトップ版・`banto-serve` など）が使用中、3 その他の起動の失敗（理由と、
  手で行う対処をコンソールとログに出す。自動では何も直さない）。起動の途中で Ctrl-C・ウィンドウを閉じると、
  DB を開く・エンジンを始めるといった段の切れ目で起動を中止する（終了コード 3）。
- **既知の制限**（設計は [docs/chronogazer-service-design.md](../../docs/chronogazer-service-design.md) §4 の
  B1-5）: Ctrl-C の後始末が 4 秒を超えている最中にウィンドウを閉じる・ログオフすると、OS の猶予（約 5 秒）の
  ため最終の書き出しが切れうる。起動の直後（PLC に接続中）にウィンドウを閉じると、`collection_stopped` が
  残らないことがある（行は失わない）。コンソールの文字を選択して出力が止まっていると、コンソールへの写しだけ
  諦める（ログには残る）。`logs\` と DB は同じ PC の他の利用者から読める（サービス化の権限の整理まで）。
  最初の管理者を作るまでの間は、同じ PC のプログラムが最初の管理者を作れ、ログインなしで管理者として入れる
  （デスクトップ版の初回セットアップと同じ範囲。ブラウザーで開いた外のページからは `Host` の確認で作れない）。

#### Windows サービスとして動かす（2026-10-15〜、#392 B3-2b）

同じ `chronogazer-server.exe` を Windows サービス `ChronoGazer`（表示名「ChronoGazer 記録エンジン」、
LocalSystem、手動開始）として登録する。サインアウトしても止まらず、OS のシャットダウンでは最終 flush と
`collection_stopped` を残してから止まる（PRESHUTDOWN、後始末は最大 24 秒）。インストーラはこれから（B3-4）
なので、今は**管理者として実行した PowerShell**（★）から手で行う。Operators・ACL・自動開始まで含めて一度に
整えるなら、下の「昇格ヘルパー `chronogazer-elev`」の `service-install` を使う（`install` と同じ登録をしてから
続きを行う）。

1. **exe を `C:\Program Files\ChronoGazer\` に置いてから登録する**。LocalSystem のサービスは SYSTEM の権限で
   動くので、一般の利用者が書き換えられる場所（`target\`・ダウンロード・デスクトップなど）の exe を登録すると、
   その利用者が SYSTEM の権限でプログラムを動かせてしまう（Program Files の外なら `install` が警告する）。

   ```powershell
   pnpm --filter chronogazer build
   cargo build --release -p chronogazer-core --bin chronogazer-server --features embed-ui
   # ★
   New-Item -ItemType Directory -Force 'C:\Program Files\ChronoGazer'
   Copy-Item target\release\chronogazer-server.exe 'C:\Program Files\ChronoGazer\'
   ```

2. ★ 登録: `& 'C:\Program Files\ChronoGazer\chronogazer-server.exe' install`。データを使う利用者のアカウント
   （画面を使う人）でサインインして実行する。その利用者が**インストール利用者**として
   `HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid` に記録され、データのルートと
   Hub の API キーの保存先の ACL に「変更」で入る（デスクトップ版とサービスで同じ Hub のキーを使える）。別の利用者に
   するときは `install --install-user-sid S-1-5-21-…`。データのルートが無ければ、ここでその利用者の ACL で作る
   （既にあれば触らない）。もう一度実行しても登録・記録は変えない（違う利用者を指定したときは警告だけ。変えるには
   `uninstall` してから `install`）。
3. ★ 開始・停止: `Start-Service ChronoGazer` / `Stop-Service ChronoGazer`（停止の後始末は最大 15 秒）。状態は
   `sc.exe query ChronoGazer`、ログは `<データ>\logs\chronogazer-server-YYYY-MM-DD.log`（コンソール版と同じ
   ファイル。サービスの行は「Windows サービス」「SCM の STOP」「PRESHUTDOWN」を含む）。動いている間は
   デスクトップ版の ChronoGazer を起動すると、プロセスの中でエンジンを動かさずにサービスの画面を開く。
4. ★ 解除: `& 'C:\Program Files\ChronoGazer\chronogazer-server.exe' uninstall`（動いていれば止めてから解除。
   インストール利用者の記録も消える。**データは残す**）。

- **終了コード**（`sc.exe query ChronoGazer` の `SERVICE_EXIT_CODE`）: 0 正常、1 後始末が時間内に終わらなかった・
  最終の書き出しに失敗（**サービス本体の panic も 1**。ログの「サービス本体が予期せず panic しました」の行で
  見分ける）、2 同じデータをデスクトップ版・コンソール版が使用中（このときはログに何も書かない。イベント
  ビューアーの System に 7024。文面は当てにしない - 下記）、3 その他の起動の失敗。`install`・`uninstall` は失敗すると終了コード 1。
  `run-service` は SCM が呼ぶもので、手で実行すると案内を出して終了コード 1。
  イベントビューアーの 7024 の文面は、終了コードを Win32 のエラー番号として読んだもので当てにならない
  （2 は「指定されたファイルが見つかりません」、3 は「指定されたパスが見つかりません」と出る）。終了コードは
  `sc.exe query ChronoGazer` の `SERVICE_EXIT_CODE` と上の意味で読む。
- インストール利用者の記録が無い・壊れているときも起動するが、SYSTEM・Administrators だけの ACL で続け、
  デスクトップ版と Hub のキーを共有できない（ログに理由と対処が出る。`install` をやり直す）。
- `CHRONOGAZER_ROOT` でデータの場所を変えるなら、**マシン全体の環境変数**にする（サービスに個別に設定すると、
  デスクトップ版が別の場所を見てサービスにつながらない）。サービスは OS の起動時の環境を使うので、変えたら OS を
  再起動する。
- 手動開始なので OS を再起動しただけでは始まらない（自動開始は下の `chronogazer-elev autostart-enable`）。

#### 昇格ヘルパー `chronogazer-elev`（2026-10-16〜、#392 B3-3b）

`chronogazer-elev.exe` は、サービスまわりの管理者の操作を**固定の 7 つのアクション**だけで行う UAC 昇格
ヘルパー（実行すると UAC の昇格を求める）。`chronogazer-server.exe` と**同じフォルダ**
（`C:\Program Files\ChronoGazer\`）に置く。登録する exe はいつも隣の `chronogazer-server.exe` で、データの
場所はマシンの環境（マシン全体の `CHRONOGAZER_ROOT`、無ければ `%ProgramData%\ChronoGazer`）から決める
（実行した人の環境変数は見ない）。

```powershell
cargo build --release -p chronogazer-elev
# ★
Copy-Item target\release\chronogazer-elev.exe 'C:\Program Files\ChronoGazer\'
```

| アクション                                        | すること                                                                                                                                                                                                                          |
| ------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `service-install [--install-user-sid S-1-5-21-…]` | `chronogazer-server install` と同じ登録 → `setup-operators` → `grant-service-acl` → `grant-data-acl`（最初の失敗で止める）。インストール利用者は指定した SID か実行した利用者（この PC の利用者のアカウントであることを確かめる） |
| `service-uninstall`                               | 登録を解除（Operators のグループとデータは残す）                                                                                                                                                                                  |
| `setup-operators`                                 | ローカルグループ `ChronoGazer Operators` を作り、記録したインストール利用者を足す（反映は次のサインインから）                                                                                                                     |
| `grant-service-acl`                               | サービス `ChronoGazer` に `ChronoGazer Operators` の照会・開始・停止の権利を足す（設定の変更・削除・権限の変更は渡さない）                                                                                                        |
| `grant-data-acl`                                  | データのルート・`secrets\`・`secrets\hub-keys\` を「SYSTEM・Administrators にフル、インストール利用者に変更、親から継承しない」にする。ジャンクション・見知らぬ所有者のフォルダは書き直さずに止める                               |
| `autostart-enable` / `autostart-disable`          | 自動開始（遅延）にする / 手動開始に戻す                                                                                                                                                                                           |

- **置き場所とデータの場所の制限**: `chronogazer-elev.exe` と `chronogazer-server.exe` が Program Files（x86 を含む）の
  中に無ければ、何もせずに終了コード 2。データの場所（マシン全体の `CHRONOGAZER_ROOT`）がドライブのルート・ネットワークの
  場所・`C:\Windows`・`C:\Program Files`・`C:\ProgramData` そのもの・`C:\Users` そのもの・`C:\Users\Public` などなら
  2（その下の専用のフォルダにする）。**`CHRONOGAZER_ROOT` は長い名前で書く**（`C:\PROGRA~1` のような
  8.3 の短い名前は、本当の場所と食い違うので断る。安全側）。`service-install` は、登録の前にデータのルートを
  （無ければ**まず SYSTEM・Administrators だけの DACL で**先に作って）確かめ、ジャンクションか見知らぬ利用者の
  持ち物なら、何も登録せずに 1（作ったフォルダが残るときはその旨を 1 行出す。利用者の「変更」は確かめが
  通ってから付く）。**無いデータのルート・`secrets\` は、確かめ済みの親のフォルダの中にだけ作る**（#642）:
  ルートの親のフォルダは**既にあること**（無ければ何も作らずに 2。既定の `%ProgramData%\ChronoGazer` では
  いつもある。`CHRONOGAZER_ROOT` を `D:\data\ChronoGazer` にするなら先に `D:\data` を作る）、ジャンクション・
  ネットワークの場所・`C:\Windows`・`C:\Program Files` などの中でないこと（なら何も作らずに 1）。親を確かめて
  から作るまでの間に親をジャンクションにすり替えられても、外にフォルダはできない。`autostart-enable` / `autostart-disable` は、登録されている実行ファイルが隣の
  `chronogazer-server.exe` と同じときだけ動く（違えば 1）。`grant-data-acl` は、データの中に利用者が
  Administrators を拒否したファイルがあると、それを触らず飛ばして 1 行出し、ほかは続ける（そのファイルは
  そのまま残る）。
- **終了コード**: 0 完了、1 失敗（書き直さずに止めたときも）、2 前提が無い（サービス未登録・インストール利用者の
  記録が無い・隣に `chronogazer-server.exe` が無い・無いデータのルートの親のフォルダが無いなど）、64 使い方の誤り。出力はどの行も `chronogazer-elev:` で始まる。
- **昇格のしかた**: 管理者の PowerShell（★）からそのまま実行するか、昇格していない PowerShell から
  `Start-Process 'C:\Program Files\ChronoGazer\chronogazer-elev.exe' -ArgumentList ... -Wait -PassThru`（UAC の
  確認が出る。出力は別の窓）。昇格していない窓から直接呼んだときは、呼び方によって「要求された操作には
  管理者特権が必要です」で何もせずに終わるか、UAC の確認を出す（どちらでも、確認なしに管理者の操作は行わない）。
- **別の管理者の資格情報で昇格する（一般の利用者の PC）ときは、自分の SID を渡す**:
  `service-install --install-user-sid ([System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value)`
  （昇格していない PowerShell で SID を取ってから渡す）。省くと、UAC で入力した管理者がインストール利用者に
  なる。直すには `service-uninstall` → `service-install --install-user-sid <自分の SID>`。
- `ChronoGazer Operators` のメンバーは、昇格せずに `sc.exe start ChronoGazer` / `sc.exe stop ChronoGazer` が
  できる（設定の変更はできない）。データのフォルダには Operators・Users の権利を付けないので、画面を使う人は
  インストール利用者にする。

### 旧形式の DB で起動しないとき（2026-10-02〜）

DB スキーマを banto の admin-template と同じ形に整理した（2026-10-02）ため、それより
前の版で作った DB では起動を拒否し、「この DB は旧形式です。<パス> を削除
または退避して起動し直してください（アルファ版のため自動移行はありません）」を
出す（デスクトップ版はメインの画面の代わりにこの文と対処を書いた小さな窓を開き、
閉じると終了する。`banto-serve` は標準エラーに出して終了する。2026-10-02 オーナー決定: 既存の DB は壊してよい。
自動移行は作らない）。アプリを終了してから、そのパスの DB を（`-wal`・`-shm`
があれば一緒に）削除または別名へ退避し、起動し直す。新しい DB はアカウントも
接続・タグも空なので、初回セットアップからやり直す。DB の場所はデスクトップ版が
データのルートの `chronogazer.sqlite3`（既定 `%ProgramData%\ChronoGazer\chronogazer.sqlite3`。
2026-10-14 より前の版は `%APPDATA%\dev.tyaro.chronogazer\chronogazer.sqlite3`）、`banto-serve` が
`BANTO_DB`（既定 `./banto-dev.sqlite3`）。

### 起動に失敗したとき（2026-10-12〜）

デスクトップ版は、起動に失敗しても落ちずにエラーの窓を出し、対処を選ばせる（#392、オーナー決定）。
**どの選択肢でも今の DB や記録は消さない**。

- 時系列データの保存先（`data.dir`）を読めない: 「保存先のフォルダを選び直す」（選んだ場所を設定に保存して起動を
  続ける）・「既定の場所に戻して起動する」（それまでの記録は元の場所に残るが画面からは見えなくなる。確認あり）・「終了」。
- 時系列データの保存先のフォルダに書き込めない（USB メモリやネットワークドライブが外れている・ドライブが無い・
  権限が無い。起動時に一時ファイルを書いて消して確かめる）: 「もう一度確かめる」（差し直す・権限を直すなどした後に
  同じ保存先でやり直す）・「保存先のフォルダを選び直す」・「既定の場所に戻して起動する」・「終了」。
- DB が壊れている・開けない: 「バックアップから戻す」（`backups\chronogazer.sqlite3\` の一覧から選ぶ。設定画面の
  リストアと同じ検査）・「新しく作り直す」（初回セットアップからやり直す。時系列データのファイルは残る）・「終了」。
  どちらも今の DB を `chronogazer.sqlite3.broken-<日時>`（`-wal`・`-shm` も同じ名前＋接尾辞）へ名前を変えて残す（確認あり）。
  他のソフトが DB を開いているなどで名前を変えられないときは、同じ窓に理由が出るので、閉じてから選び直す
  （権限・ロックで開けないときも「作り直す」は出るが、ロック中なら名前の変更が失敗するだけで何も消さない）。
  同じ DB を `banto-serve` や別の ChronoGazer が使用中なら、作り直す・戻すは何も変えずに理由を出す（先に止めてから選び直す）。
  バックアップを適用できなかったときは、元の DB を元の名前に戻して理由を出す（空の DB では起動しない）。
- 新しい版の ChronoGazer が使った DB、その他の失敗: 「終了」だけ（理由を出す。ファイルには触れない）。
- 既に起動中・旧形式の DB: 上の各節の窓（今までどおり）。

選んだ対処は監査ログに `startup_recovery` として残る（バックアップから戻したときは `restore_applied` も）。
デスクトップ版はログファイルを残さないので、問い合わせには窓の「詳しい理由」を控える。

### 1 つの DB につき 1 プロセス（2026-10-10〜）

デスクトップ版と `banto-serve` は、起動時に収集を自動開始するため、**同じ DB を同時に 2 つのプロセスで
開けない**（開くと 2 つの収集エンジンが同じ時系列ファイルに書き込んで壊れる）。DB を開く前に OS の排他
（Windows は名前付き mutex `Global\ChronoGazer.<DB パスのハッシュ>`、それ以外は DB の隣の
`<DB のファイル名>.instance.lock` への `flock`）を取り、取れなければ DB に触れずに終了する。
デスクトップ版は「ChronoGazer は既に起動しています」の小さな窓（閉じると終了。使っているのが
`chronogazer-server` のときは「この PC で ChronoGazer のサービス（またはコンソール版）が同じデータを使っています」）、`banto-serve` は標準エラーに
理由を出して非 0 で終了する。**DB が違えば同時に動かせる**（E2E が一時ディレクトリごとに `banto-serve` を
立てるのはこのため）。プロセスが落ちても OS が排他を返すので、ロックが残って起動できなくなることはない
（`.instance.lock` ファイルが残っていても無害）。別の DB は同時に動かせるが、**解決した `data.dir` が同じ**（相対でも絶対でも。同じフォルダに
置いた 2 つの DB で `data.dir` が既定のままだと、どちらも `<フォルダ>/data` になる）と二重書き込みは防げない。
DB ごとに専用のフォルダか `data.dir` を割り当てること。DB ファイルのパス自体がシンボリックリンク（壊れたリンクを含む）だと、ロックの単位と実際に開く DB がずれうるので起動を断る（実体のパスを指定する。親フォルダのリンクは可）。
ハードリンクの別名は検出しない。

デスクトップ版をもう一度起動したとき（同じログオンセッション）は、この排他より先に
`tauri-plugin-single-instance` が受け、既に動いている ChronoGazer のウィンドウを前に出して 2 つ目は
何もせずに終わる（2026-10-11〜、#392 A2）。上の「既に起動しています」の窓が出るのは、`banto-serve`・
`chronogazer-server` が同じ DB を使っているときや、別のログオンセッションから同じ DB を開こうとしたとき
（2026-10-14〜 DB は PC に 1 つの `%ProgramData%\ChronoGazer\` なので、別の利用者のデスクトップ版とも当たる）。

### ウィンドウを閉じても収集は続く（トレイ常駐、2026-10-11〜）

デスクトップ版のウィンドウの × は**アプリを終了せずにウィンドウを隠す**（#392 A2）。収集・Hub の購読・
LAN アクセスはそのまま動き続け、アプリはタスクトレイに残る（初回だけ OS の通知で知らせる。既読の印は
`%APPDATA%\dev.tyaro.chronogazer\tray-hint-shown`。データのルートが変わった後もここのまま）。

- トレイのアイコンの**左クリック**か、右クリックのメニューの「開く」でウィンドウを戻す。メニューの 1 行目は
  収集の状態（2 秒ごとに読み直す）。
- アプリを終わらせるのはメニューの「**終了**」。収集中（開始中を含む）なら「収集を停止してから終了」の
  確認が出る。同意すると、従来の終了処理（LAN サーバー → 収集の停止と最終 flush・`collection_stopped`
  → Hub の購読 → DB）を 1 回だけ通してから終了する。停止に失敗した・5 秒以内に終わらなかったときは、
  その旨を出してから終了する（最後の値の一部が書き出されていない可能性がある）。
- Windows のシャットダウン・ログオフでも同じ終了処理を通す。
- banto-hub のデスクトップ版のトレイも同じ操作（左クリックでウィンドウを戻す、右クリックでメニュー）。
- **ログオン時の自動起動は無い**。アプリを起動しない限り収集しないのは従来どおり（UI から独立した
  サービスでの収集は #392 の B で扱う。画面の無いコンソール版は上の `chronogazer-server`）。

## 起動時の判定と「サーバーに接続できません」（2026-10-04〜）

画面を開いたとき、どの環境で動いているかを次の順で決める（banto v3.0.0 の
admin-template と同じ形。banto #286、`src/lib/banto/environment.ts`・
`startup.ts`・`setup.ts`）:

1. デスクトップ（Tauri）の窓なら、そのままデスクトップの経路。
2. `VITE_BANTO_DEMO=1` でビルドした画面なら、サーバーに問い合わせずに demo
   （メモリ上の空データ、demo のログイン）。**ChronoGazer には今のところ demo を
   静的に公開する経路（GitHub Pages など）が無い**ので、このビルドは使っていない。
3. それ以外（LAN モードのブラウザ・`banto-serve`）は `GET /api/auth/check` に問い合わせる。
   - サーバーが答えれば実データの画面（server）。
   - 同じオリジンが「`/api` は無い」と確定的に答えた（`404`/`405`/`410` で Banto の
     本文が無い。静的ホストや `vite dev`/`vite preview`）ときだけ demo。
   - **届かない**（サーバーが止まっている・タイムアウト 5 秒・逆プロキシの `502`/`503`
     など）ときは **demo に落とさない**。「起動中…」のまま 1.5 秒おきに 2 回まで
     自動で問い合わせ直し、それでも届かなければ「サーバーに接続できません」と
     「再接続」ボタンを出す。接続できるまで画面は開かない。

以前は届かないときも demo に落ち、LAN の画面が空のデータのまま戻らなかった。
サーバーを起動し直してから「再接続」を押すと、実データの画面に入る。

**保護画面を直接開いたとき（banto v3.0.1 の #321 で解消）**: `/`・`/monitor` などログインの
必要な画面を直接開いても、届かない間は「起動中…」→「サーバーに接続できません」＋「再接続」が
出る。ルートガード（`routes/(app)/+layout.ts`）は起動の判定を待たず（待つと SvelteKit が
最初の load の完了まで何も描かないため真っ白になっていた）、終わっていなければ起動待ちの印付きの
503 を投げて延期する（`startupGate.ts`）。ルートのレイアウトはその間エラー画面ではなくスプラッシュを
出し、起動の判定が終わってから `refreshAll()`（SvelteKit 3。以前の `invalidateAll()`）で同じ URL を
開き直す。やり直しはナビゲーションが終わってから始める（banto v4.0.0 の #326、
`navigationSettled.svelte.ts`。保護レイアウトの「セッションが変わったら load をやり直す」も同じ条件で待つ）。保護ルートの子の
`load` は `await parent()` してから provider に触れること（この順序がガードより前に子の load を
動かさない条件）。

## ログインとセッション

ログイン（デスクトップの窓・LAN モードのブラウザ、「ログインしたままにする」
を含む）は、操作のたびにアカウントと照合される（banto v1.7.0 #204、
2026-09-24 追従）。ユーザーの削除・降格・パスワードの変更やリセットをすると、
そのアカウントのほかの端末・ほかの窓のセッションは次の操作で切れてログイン
画面に戻る。自分でパスワードを変更した画面のセッションは続く。照合中に DB が
応答しないときは、セッションを残したままその操作だけが失敗する（ログイン
画面には移らず、再試行できる）。

「ログイン不要モード」（設定の「セキュリティ」）の合成セッションは、モードが
ON の間だけ有効で、OFF にするとその場でセッションが終わりログイン画面に
なる（banto v2.0.0）。ON にすると、ログイン中のアカウントのセッションはその場で
合成セッション（設定した役割）に置き換わる。合成セッションからはパスワードを
変更できない。

別の窓・タブで別のユーザーがログインすると、開いている画面はそのユーザーで
開き直され、その旨が通知される（banto v2.0.0 の SessionController。ログアウトで
ログイン画面へ移れなかったときも通知する）。

## アカウント・監査ログ・バックアップ（banto のもの、2026-10-04〜）

アカウント（`users`）・設定（`settings`）・監査ログ（`audit_log`）・バックアップの
サービスと、その REST の口（`/api/auth/*`・`/api/users/*`・`/api/audit-log/*`・
`/api/backups/*`・`/api/ui-settings/*`）は、banto の `banto-admin-services` と
`banto_server::routes` のものをそのまま使う（以前は admin-template からのコピーを
持っていた。独自実装は banto に寄せる、2026-10-01 オーナー方針）。ChronoGazer 側に
残っているのは、固有の設定キー（`data.dir`・`retention.days`、`hub.*`）の型付きの
読み書きと、Hub・収集・タグレジストリの口だけ（`core/src/settings.rs`・
`core/src/rest.rs`）。これで banto v2.1.0 の次の修正が入った:

- 初回セットアップは 1 つしか成功しない（同時に送っても管理者は 1 人。banto #277）。
- 失敗したログインの監査ログのユーザー名は 32 文字までに切り詰める。トークン無し・
  無効なトークンのログアウトは監査ログに記録しない（banto #278）。
- バックアップとリストアの予約は DB ファイルごとのフォルダ
  `<DB のフォルダ>/backups/<DB ファイル名>/`（デスクトップ版は
  `%ProgramData%\ChronoGazer\backups\chronogazer.sqlite3\`。2026-10-14 より前の版は
  `%APPDATA%\dev.tyaro.chronogazer\backups\chronogazer.sqlite3\`）に置く（banto #280）。
  **それより前の版で作ったバックアップ**（`<DB のフォルダ>/backups/` 直下の
  `*.sqlite3`、`<DB のフォルダ>/restore-pending.sqlite3`）は一覧に出ず、自動では
  適用されない。起動時に標準エラーへ警告が出るだけで、移しも消しもしない
  （2026-10-04 オーナー決定: banto の既定どおり）。使いたいときは手で上のフォルダへ
  移す。

## DB に書けなかった監査の保留（#437、2026-10-10〜）

監査（`audit_log`）の書き込みは最大 3 秒で打ち切り、DB に書けない（表が壊れている・
ディスクが満杯・接続を握られて応答しない）ときは、監査を **DB ファイルの隣の
`audit-spool/<DB ファイル名>/`** に 1 件 1 ファイルで退避して、操作はそのまま成功させる
（banto v6.5.0 の監査の保留、banto ADR-0019。banto-hub と同じ仕組み）。デスクトップ版では
`%ProgramData%\ChronoGazer\audit-spool\chronogazer.sqlite3\`（2026-10-14 より前の版は
`%APPDATA%\dev.tyaro.chronogazer\audit-spool\chronogazer.sqlite3\`）、`banto-serve` では
`BANTO_DB` のフォルダ（既定 `./audit-spool/banto-dev.sqlite3/`）。**DB ファイルごとに
分ける**のは、同じフォルダの `a.sqlite3` と `b.sqlite3` が保留を共有すると、B の起動時の
流し込みが A の監査を B に入れて消すため（バックアップの `backups/<DB ファイル名>/` と
同じ分け方）。置き場所は時系列データの `data.dir` ではなく DB の隣
（`data.dir` は設定で動かせて、動かすと保留が取り残されるため）。DB が戻ると、
起動時・監査の書き込みの成功時・30 秒ごとに `audit_log` へ流し込む（`detail.spooled =
true`。時間切れの書き込みが遅れて完了しても、同じ `pending_id` で 1 行にまとまる）。

- 状態を読めなかったとき（DB が応答しないときなど）は「保留の状態を読めませんでした」と
  出す（0 件とは区別する）。溜まっている間（と、上限 10,000 件を超えて捨てた分・保留にも書けずに失った分が
  あるとき）、**監査ログ画面（admin）の上部に警告帯**が出る。同じ値は
  `GET /api/audit-log/spool`（admin、`pendingCount` / `pendingOldestTs` /
  `droppedCount` / `failedCount`）と Tauri の `audit_spool_status` で読める。
  `dropped` / `failed` はそのプロセスの起動後の累計。
- 保留ディレクトリを作れない・読めないときは、起動を止めずに保留なしで続け、標準
  エラー出力に警告を出す。ChronoGazer にはログファイルが無いので、banto-hub と違って
  警告行は標準エラー出力のまま（`with_log_sink` は付けていない）。
- 中身は監査の内容そのもの（`detail` を含む）。データディレクトリと同じ扱いで守る。

## LAN アクセスと閲覧公開（2026-10-04〜）

設定の「接続」で、デスクトップ版の組み込み Web サーバー（LAN アクセス）を有効に
できる（管理者のみ）。「保存して適用」は banto v2.1.0 の形（banto #287・#288・#294）:
新しいポートで待ち受けられることを確かめてから保存し、それから応答を始める。
**ポートが使用中などで失敗したときは何も保存せず、前の設定でサーバーを起こし直す**
（保存値と実際の稼働が食い違わない）。失敗も監査ログに `settings_change`（結果
`failed`、`saved: false`）として残り、画面はエラーを出して実際の状態を読み直す。

**閲覧公開**（「ログイン無しで LAN から閲覧を許可する」）を ON にすると、LAN の
ブラウザはログインせずに閲覧者（`viewer`）として監視・ヒストリカル・イベントを
見られる（2026-10-04 オーナー決定。同日の I2a で「画面に出さない」としたのを変更）。
ログイン画面にも「閲覧のみで続ける」が出る。書き込みや管理画面（タグ設定・
ユーザー管理・監査ログ・設定）にはログインが要る（閲覧者のセッションのナビには
出さず、開こうとすると監視へ移る。読める API は viewer の権限のまま。この画面の絞り込みは
`(app)/+layout.ts` のガードが `url.pathname` と `resolveAppPath()` で解決した許可リストを比べて行う）。手元の
端末はログイン不要モード、LAN の相手には閲覧だけ、という使い方ができる:
「ログイン不要モード + LAN アクセス」は閲覧公開が ON のときだけ許され、起動時の
自動開始も同じ条件で判断する。閲覧公開を OFF にして適用すると、発行済みの
閲覧者のセッションはその場で切れる（ログインしているアカウントのセッションは
残る）。仕組みは banto の grant（ADR-0012・ADR-0017、`POST
/api/auth/grant/publicViewer`）。信頼できる LAN でのみ有効にすること。

組み込みサーバー（デスクトップ版の LAN アクセスと `banto-serve`）の応答には、
banto の**セキュリティヘッダー**（CSP・`X-Content-Type-Options: nosniff`・
`X-Frame-Options: DENY`・`Referrer-Policy: same-origin`。banto の
`with_security_headers`、#500）が付く。CSP は `connect-src 'self'` などで同一
オリジンの通信だけを許す（admin-template と同じ）。

デスクトップ版の窓（WebView）にも、同じ CSP に Tauri の IPC（`connect-src` の
`ipc: http://ipc.localhost`）を足したものを掛けている（`src-tauri/tauri.conf.json` の
`app.security.csp`、#505。admin-template と同じ）。2 つは揃えておくこと
（`src-tauri` のテスト `window_csp_is_the_served_csp_plus_tauri_ipc` が banto の応答の
CSP と突き合わせる）。Tauri はビルド時に HTML のインライン script のハッシュ（と
`<style>` の nonce）を CSP に足すので、`script-src` の `'unsafe-inline'` は実際には
効かない（インライン script は `app.html` の先頭のテーマ設定と SvelteKit の起動処理、
`static/legacy-db.html` の 1 つで、どれもハッシュで通る）。Hub への接続は Rust 側で
行うので、窓から Hub へ直接つなぐことはない。**`tauri dev`（Vite の devUrl）では窓の
CSP は掛からない**（Tauri は自前のアセットの応答にだけ CSP を付ける）ので、CSP の
確認はリリース相当のビルド（`pnpm tauri build --no-bundle`、手早くは `--debug` も可）で
行う。

## Hub のタグを登録する（#383 段階3 P4）

banto-hub のタグを ChronoGazer で記録するには、Hub のタグを ChronoGazer のタグとして**登録**する。

1. 管理者が「設定 > Hub接続」で Hub に接続する（接続先 URL を入れて「接続」。試運転中の Hub なら
   読み取り用の API キーを自動で発行する。ロックダウン済みの Hub は管理者が発行したキーを採用する）。
2. 編集者以上が「タグ設定」のタグの節の **「Hub から追加」** を押す。Hub のタグ一覧が出るので、検索して
   チェックし、登録先の収集グループ（Hub 接続の下の既存のグループか、名前と収集周期を入れて新しく作る。
   周期の既定は 1 秒）を選んで「N 件を登録」を押す。
   - 名前・データ型・単位・小数桁は Hub のタグから複製し、アドレスは Hub のタグ名（外部名
     `接続名.グループ名.タグ名`）になる。書き込みはできず、スケーリングも設定できない（Hub は工学値を送る）。
   - 登録できるのは数値と bit だけ（文字列などは理由付きで選べない）。登録済みのタグは「登録済み」と出る。
   - 登録先のグループに同じ名前のタグがあると、その行にエラーが出て**何も登録しない**（自動で改名はしない）。
   - 最初の登録のとき、タグ設定の PLC 接続に「Hub」（Hub 経由の接続）が自動で作られる。この行は読み取り
     専用で（接続先とキーは「設定 > Hub接続」）、配下にグループがあるうちは削除できない。
     「有効」を外して保存すると、その Hub 接続のタグは購読も収集もしない（収集中は「収集を再起動」で外れる）。
3. 収集中なら「収集を再起動」で記録が始まる（他のタグの編集と同じ）。

登録した Hub のタグは、収集していなくても購読され、「設定 > Hub接続」の「登録済みのHubタグ」に現在値が出る。
**登録を変える（追加・削除）と購読を 1 回張り直す**ので、収集中は数周期のあいだ Hub 経由のタグが不良（Bad）
になり、Hub の接続で切断・再接続のイベントが記録される（収集の開始・停止では張り直さない）。
以前の版の「設定 > Hub接続」で選んでいたタグは、自動ではタグにならない。「Hub から追加」を開くと
1 度だけあらかじめチェックされ、登録するか「以前の選択を使わない」を押すと消える。

## Hub 接続：キーが使えなくなったとき

Hub 経由のタグを banto-hub から購読しているとき、Hub が
「この API キーはもう使えない」と言って購読を打ち切ることがある（Hub は
開いている購読のキーを 15 秒ごとに確かめ直している）。画面の「購読」欄は
理由ごとに次の一手を案内する（#446）:

| 理由                       | 画面の案内                                                                                       | アプリの動き                                                                                     |
| -------------------------- | ------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------ |
| トリップ（一時停止）       | Hub の管理者に解除を依頼してください                                                             | 2 分おきに自動で確かめ、解除されたら**同じキーのまま**再開する。この画面を開き直すとすぐ確かめる |
| 失効・期限切れ・存在しない | 新しい API キーを設定してください（「接続」で再発行、または Hub の管理画面で発行したキーを採用） | 同じキーでは戻らないので**自動では再試行しない**。キーを設定し直すと購読が再開する               |
| 理由を判別できない         | Hub の管理者に確認してください                                                                   | 2 分おきに自動で確かめる                                                                         |

トリップ中は接続の状態も「キーがトリップ中」になり、キーの入力欄は出さない（キーを捨てさせない）。
詳しくは [../../docs/banto-hub-client-bootstrap.md](../../docs/banto-hub-client-bootstrap.md) §5 / §10.6。

## PLC 側の設定（SLMP、三菱 iQ-R）

ChronoGazer は接続ごとに SLMP の 1 ポートへ TCP でつなぐ。2026-10-09 の実機の確認（R08ENCPU）で、LAN ケーブルを
抜いたあとの復旧に約 11 分かかった。原因は PLC 側の設定なので、試運転の担当者が次を確かめる。

- **ポートは接続ごとに分ける**: 検証機（R08ENCPU、Ethernet 部（形名 RJ71EN71）・FW 86）では、3101 は再接続を拒否し、3102 は
  受け付けた（観察した事実）。三菱電機の FW 更新履歴では、R08ENCPU のネットワーク部 78 以降で同じ TCP ポート番号に複数の
  SLMP 機器をつなげるようになっている。ただし検証機は FW 86（78 以降）でも同じポートへの再接続を拒否したので、同じポートで複数接続を受けるかは FW の版だけでなく設定にもよる。FW の版と設定は試運転で確かめる。
  **方針（2026-10-09 オーナー決定）: FW が同じポートでの複数接続を許していても、記録計（ChronoGazer）や SCADA は
  ほかのシステム（タッチパネル・ほかのアプリなど）と同じポートで接続しない（専用のポートを割り当てる）。** ChronoGazer の接続ごとに別のポートを割り当て、タッチパネルや他のアプリとも分ける。
- **生存確認（KeepAlive）を有効にする**: GX Works3 の Ethernet 構成で、使う SLMP ポートの「生存確認」を KeepAlive にする。
  無効だと、ケーブルの抜けなどで切れたセッションを PLC がいつまでも持ち続け、そのポートが復旧しない。
- **ポートが空くまでの時間**（PLC が古いセッションを持ち続けるときの話）: ケーブルを抜く・スイッチが落ちるなどすると、
  PC 側は TCP 接続を切るが、検証機の PLC は古いセッションを持ち、同じポートへの再接続を拒否した。CPU パラメータの「データ交信用のタイマ設定」の「相手先 生存確認開始間隔タイマ」
  （検証機は 600 秒）の間は何も確かめず、その後「生存確認間隔タイマ」（10 秒）×「再送回数」（3 回）で切る。検証機では
  約 600 + 10×3 秒 ≈ 10.5 分、ポートが占有されたままだった。ChronoGazer の再接続の待ちは上限 30 秒なので、ポートが空いてから
  30 秒以内につながる。
- **短縮の目安**: PLC が古いセッションを持ち続ける構成で、断線からの復旧を早めたい現場では、開始間隔タイマを 30〜60 秒程度に短くすることを勧める。ただし、
  生存確認の通信が増えること、負荷の高いネットワークで誤って切断するおそれがあることとの兼ね合いなので、値は試運転の担当者が決める。
- **見分け方**: PLC に届いていて、ChronoGazer が「通信エラー」のまま接続が「再接続中」で戻らず、別のポートなら受けるときは、
  この占有が疑わしい。生存確認のタイマが切れるまで待つか、PLC 側でそのポートを再起動（Ethernet ポートの再初期化）する。

## 関連ドキュメント

- 要件定義: [../../docs/recorder-requirements.md](../../docs/recorder-requirements.md)
- 実施計画（R1-A〜R1-D）: [../../docs/r1-plan.md](../../docs/r1-plan.md)
- 全体計画（R系マイルストーン）: [../../docs/plan.md](../../docs/plan.md) §4
