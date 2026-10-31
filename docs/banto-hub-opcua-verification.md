# banto-hub OPC UA 検証

状態: **自動 interop smoke 実装・Linux 実接続検証済み。Windows Service と UaExpert の実機確認も済み（2026-11-01、下の「手動試験記録」。全項目が期待どおり）。**
最終更新: 2026-11-01（#201・#943: Windows Service と UaExpert の手動試験を、S7 の機材の BantoHub サービスを別の profile に切り替えて行い、記録の表を埋めた。サービスの鍵と証明書の置き場の ACL と、再起動での再利用も記録した。専用 VM でない PC で行う手順を「既にサービスがある PC で行うとき」に足した）。2026-10-17（#634: 有効化の PUT は起動の完了を待たずに返るので、手順 4 に状態の API での確認を足した。#636: 手動確認の手順 7 の文言の重複を直し、Machine 環境変数を消す後始末を手順 8 として追加）。2026-10-16（#201、Hub #627 マージ後の外部クライアント試験を追加）。

ローカル確認: Hub `b130385`（`0.2.0-alpha.31`）、Rust 1.98.1、Python 3.12、asyncua 1.1.8。新しい CI ワークフローでの結果は PR のチェックを参照する。

この文書は OPC UA Phase 2 の外部クライアント検証手順と結果記録の正。自動試験は Hub の REST API で試験専用ユーザー・接続グループ・internal 数値タグ・API キーを作り、別プロセスの Python `asyncua==1.1.8` から実際の OPC UA listener に接続する。固定テストサーバーや同梱 Rust クライアントは使わない。

## 自動 smoke の範囲

`interop/opcua/smoke.py` は指定された `banto-hub` バイナリだけを起動し、専用の一時ディレクトリ、SQLite DB、data directory、loopback HTTP/OPC UA ポートを使う。REST で setup/login と CSRF ヘッダーを通し、予約済み `mem` 接続を確認し、group/tag/API key を作成して収集を開始する。OPC UA の有効化の PUT は起動の完了を待たずに返るので、`GET /api/opcua-status` が `running` になり、案内された endpoint が接続先と一致することを確かめてから接続する（#634）。API キーは一時 DB 内だけに保存され、出力しない。終了時は SIGINT による正常停止を確認し、例外時の cleanup に限って停止しない子を kill して reap する。

試験は GetEndpoints、Objects からタグまでの Browse、Good な数値 Read、REST 書き込みに対する Subscription DataChange、2 回 Read での SourceTimestamp 保持、OPC UA Write の拒否と値保持を確認する。続けて Hub を正常停止・再起動し、設定と同じ NodeId が復元されることを確認する。再起動後の internal 値は REST で再投入する。

リポジトリルートから実行する:

```sh
cargo build --locked -p banto-hub-host --bin banto-hub
python3.12 -m venv /tmp/banto-hub-opcua-interop-venv
/tmp/banto-hub-opcua-interop-venv/bin/python -m pip install -r interop/opcua/requirements.txt
/tmp/banto-hub-opcua-interop-venv/bin/python interop/opcua/smoke.py target/debug/banto-hub
```

Linux 専用の `.github/workflows/opcua-interop.yml` は Python 3.12 と固定 requirements で同じ smoke を実行する。`workflow_dispatch`、main への関連変更、関連 PR で起動する独立ジョブであり、必須 `ci-gate` には加えない。

この自動試験は Windows Service、UaExpert、OPC Foundation CTT、実 PLC/実ネットワーク機器を検証しない。これらの手動結果は以下の記録表へ記入する。

## Windows Service と UaExpert の手動確認

本試験には **BantoHub サービスが未登録で実ユーザーデータのない専用 Windows VM** を使う。既存サービスや利用者データのある端末では実施しない。サービス名は `BantoHub` 固定で、インストールした PowerShell の環境変数が SCM 起動環境へ保存される保証はない。`BANTO_HUB_ROOT` と `BANTO_HUB_PROFILE` を含むサービス環境は、隔離された VM 上でシステム環境として設定してから再起動し、サービスを起動する。環境変数の設定だけで同居中の既存サービスを隔離できるとは扱わない。

1. ビルド済み `banto-hub.exe` と UaExpert を専用 VM に配置し、管理者 PowerShell で BantoHub サービスが未登録であることを確認する。
2. 管理者 PowerShell で以下を実行し、試験専用の **Machine 環境変数**を設定する。この設定は専用 VM 全体に適用される。`$env:...` の代入だけでは SCM の環境には反映されない。

   ```powershell
   [Environment]::SetEnvironmentVariable('BANTO_HUB_ROOT', 'C:\BantoInterop', 'Machine')
   [Environment]::SetEnvironmentVariable('BANTO_HUB_PROFILE', 'interop-201', 'Machine')
   [Environment]::SetEnvironmentVariable('BANTO_BIND', '127.0.0.1', 'Machine')
   [Environment]::SetEnvironmentVariable('PORT', '8722', 'Machine')
   [Environment]::SetEnvironmentVariable('BANTO_ALLOW_SETUP', '1', 'Machine')
   ```

3. 実装済みのサービス登録コマンド `.\banto-hub.exe install` を管理者 PowerShell から実行する。登録コマンドを実行したシェルの環境が SCM に保存されるとは仮定しない。VM を再起動し、専用のシステム環境がサービスに渡ることを確認してから `Start-Service BantoHub` で開始する。
4. 初回 setup を行い、その後 `[Environment]::SetEnvironmentVariable('BANTO_ALLOW_SETUP', $null, 'Machine')` で初回 setup 用の環境変数を削除して VM を再起動する。サービス起動後に管理者として login し、internal 数値タグを作って収集を開始し、試験用 API キーの `write:mem.<group>.<tag>` スコープで初期値を書き込む。REST 専用ビルドには UI がないため、UI で操作する場合は `embed-ui` を含む配布ビルドを使う。REST の OPC UA 設定で有効化し、bind は `127.0.0.1` にする。有効化の PUT は起動の完了を待たずに返るので、`GET /api/opcua-status` の `state` が `running` になる（`failed` なら `error` に理由が出る）のを確かめてから次へ進む。UI では設定画面の OPC UA の欄に同じ状態が出る。
5. UaExpert から `opc.tcp://127.0.0.1:<設定ポート>/` へ接続する。Security Policy None、Anonymous を選び、`Objects/Banto/mem/<group>/<tag>` を開く。
6. タグが数値 `Double` と Good status で読めることを確認する。REST から値を変更し、UaExpert の subscription が新値を通知すること、変更がない間の Read で SourceTimestamp が保持されることを確認する。UaExpert から値を書こうとしたとき拒否され、値が変わらないことを確認する。
7. OPC UA 設定を確認してサービスを再起動し、同じ endpoint と NodeId で接続できることを確認する。試験後は `Stop-Service BantoHub`、`.\banto-hub.exe uninstall` の順で停止・登録解除し、試験 VM を破棄する。
8. 試験 VM を破棄せずに再利用する場合は、手順 2 で設定した Machine 環境変数を管理者 PowerShell で削除する。途中で中断したときも同じく片付ける（手順 4 の前に中断すると `BANTO_ALLOW_SETUP` も残っているので、5 つすべてを消す）。

   ```powershell
   'BANTO_HUB_ROOT','BANTO_HUB_PROFILE','BANTO_BIND','PORT','BANTO_ALLOW_SETUP' | ForEach-Object {
     [Environment]::SetEnvironmentVariable($_, $null, 'Machine')
   }
   ```

   この手順も、専用 VM・実データなしの前提は変えない。既にこれらの変数を持っていた VM を再利用する場合は、手順 2 の前に元の値を記録しておき、削除ではなく元の値に戻す。

### 既にサービスがある PC で行うとき（2026-11-01、オーナー決定）

2026-11-01 の試験は、専用 VM ではなく S7 の機材（BantoHub サービスが入っている PC）で、**サービスを試験専用の profile に切り替えて**行った。本番の profile には触れない。Machine の環境変数は使わず、サービスの `Environment`（レジストリの複数行の文字列）に足して戻す。

1. 管理者 PowerShell で今の `Environment` をファイルに控え（`(Get-ItemProperty HKLM:\SYSTEM\CurrentControlSet\Services\BantoHub).Environment`）、サービスを止める。
2. **先に `banto-hub-elev.exe grant-profile-acl <利用者> <試験の profile-id>` を実行する。** 無い profile では戻す相手の記録（`Owners`）が無く、サービスは運用モードにできずに終了コード 4 で止まる（イベント 7024 の「ファイルを開くことができません」は 4 を Win32 のエラーとして読んだ文面）。
3. `Environment` に `BANTO_HUB_PROFILE=<試験の profile-id>` と `BANTO_ALLOW_SETUP=1` を足して起動する。初回 setup の後に `BANTO_ALLOW_SETUP` を外して起動し直す。以降は手順 4〜6 と、手順 7 の再起動と同じ endpoint・NodeId での再接続の確かめだけを行う。**手順 7 の `Stop-Service` の後の `.anto-hub.exe uninstall` と VM の破棄、手順 8 は行わない**（`uninstall` は本番と共有のサービスの登録を消すので、次の手順で `Environment` を戻しても本番に戻らない）。試運転モードの間は、loopback から `POST /api/auth/grant/commissioning`（ヘッダ `X-Banto-Client: banto`）で REST を操作できる。
4. 終わったら、控えた `Environment` をそのまま書き戻して起動し、`Environment` の読み戻しと、サービスログで本番の profile の `DB at`・待ち受けを確かめる。試験の profile は残る（消すかは別に決める）。

## 手動試験記録

| 項目                                 | 期待値                                     | 実測結果                                                                                                                                                                                                                                                                                                                                                                                                   |
| ------------------------------------ | ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ビルド SHA / バージョン              |                                            | `b2d3e75c`・`0.3.0-alpha.1`（S7 の版。`banto-hub.exe` の SHA256 `e4c82ee1…`）                                                                                                                                                                                                                                                                                                                              |
| Windows VM / OS build                | 専用・実データなし                         | 専用 VM ではない。S7 の機材 `MSI-A13`、Windows 11 Pro 10.0.26300。試験専用の profile `interop-201`（実データなし）に切り替えて行い、本番の profile `default` には触れていない（上の「既にサービスがある PC で行うとき」）                                                                                                                                                                                  |
| 実施者 / UTC 日時                    |                                            | オーナー（管理者の操作・UaExpert）と Claude（REST・読み戻し）/ 2026-10-31 17:30〜17:44 UTC                                                                                                                                                                                                                                                                                                                 |
| Service install/start/stop/uninstall | `BantoHub` が起動・停止し登録解除できる    | 起動・停止・再起動は可。登録・解除はしていない（S7 のサービスを使ったため。インストーラでの登録は 2026-10-31 の P-2 で済み）。最初の起動は `grant-profile-acl` の前で、終了コード 4 で止まった（期待どおりの断り）                                                                                                                                                                                         |
| UaExpert endpoint / security         | endpoint に接続、Security None / Anonymous | `opc.tcp://127.0.0.1:4840/`、None / Anonymous で接続                                                                                                                                                                                                                                                                                                                                                       |
| Browse / Read / status               | 指定タグを発見、数値 Double、Good          | `Objects/Banto/mem/interop/temp1`（`ns=2;s=mem.interop.temp1`、internal・f32）を発見。`Double`・12.5・Good                                                                                                                                                                                                                                                                                                 |
| REST 更新 / DataChange               | 新値を通知、Good                           | REST で 20.25 を書くと Subscription に 20.25・Good が届いた。SourceTimestamp は REST の書込みの時刻                                                                                                                                                                                                                                                                                                        |
| SourceTimestamp                      | 値変更なしの連続 Read で同値               | 連続の Read で同値。ServerTimestamp も値の変更の時刻のまま進まなかった                                                                                                                                                                                                                                                                                                                                     |
| UaExpert Write                       | 拒否、元値を保持                           | `BadUserAccessDenied` で拒否。REST の読み戻しで値と時刻は元のまま                                                                                                                                                                                                                                                                                                                                          |
| Service 再起動 / NodeId              | 同じ endpoint と NodeId で再接続           | 再起動の後、UaExpert が自分で同じ endpoint・NodeId に再接続。値は retain なしの internal のため未取得の Bad になり、REST で書き直すと Good                                                                                                                                                                                                                                                                 |
| 備考・ログ参照                       | 秘密値を含まない記録先                     | #943 の鍵の置き場（サービス、LocalSystem）: `data\opcua\pki` は保護された ACL で SYSTEM・Administrators のフルだけ。`private`・`private.pem`・`own\cert.der` はそれを継承した同じ 2 つだけ。記録上の利用者（管理者でない）からは置き場も鍵も開けない。再起動の前後で鍵と証明書の SHA256 が一致（作り直していない）。サービスログは `C:\ProgramData\BantoHub\profiles\interop-201\logsanto-hub-service.log` |
