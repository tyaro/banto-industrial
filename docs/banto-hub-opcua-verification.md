# banto-hub OPC UA 検証

状態: **自動 interop smoke 実装・Linux 実接続検証済み。Windows Service と UaExpert の実機確認は未実施。**
最終更新: 2026-10-17（#634: 有効化の PUT は起動の完了を待たずに返るので、手順 4 に状態の API での確認を足した。#636: 手動確認の手順 7 の文言の重複を直し、Machine 環境変数を消す後始末を手順 8 として追加）。2026-10-16（#201、Hub #627 マージ後の外部クライアント試験を追加）。

ローカル確認: Hub `b130385`（`0.2.0-alpha.31`）、Rust 1.98.1、Python 3.12、asyncua 1.1.8。新しい CI ワークフローでの結果は PR のチェックを参照する。

この文書は OPC UA Phase 2 の外部クライアント検証手順と結果記録の正。自動試験は Hub の REST API で試験専用ユーザー・接続グループ・internal 数値タグ・API キーを作り、別プロセスの Python `asyncua==1.1.8` から実際の OPC UA listener に接続する。固定テストサーバーや同梱 Rust クライアントは使わない。

## 自動 smoke の範囲

`interop/opcua/smoke.py` は指定された `banto-hub` バイナリだけを起動し、専用の一時ディレクトリ、SQLite DB、data directory、loopback HTTP/OPC UA ポートを使う。REST で setup/login と CSRF ヘッダーを通し、予約済み `mem` 接続を確認し、group/tag/API key を作成して収集を開始する。OPC UA の有効化の PUT は起動の完了を待たずに返るので、`GET /api/opcua-status` が `running` になり、案内された endpoint が接続先と一致することを確かめてから接続する（#634）。API キーは一時 DB 内だけに保存され、出力しない。終了時は SIGINT による正常停止を確認し、例外時の cleanup に限って停止しない子を kill して reap する。

試験は GetEndpoints、Objects からタグまでの Browse、Good な数値 Read、REST 書き込みに対する Subscription DataChange、2 回 Read での SourceTimestamp 保持、OPC UA Write の拒否と値保持を確認する。続けて Hub を正常停止・再起動し、設定と同じ NodeId が復元されることを確認する。再起動後の internal 値は REST で再投入する。

リポジトリルートから実行する:

```sh
cargo build --locked -p banto-hub-core --bin banto-hub
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

## 手動試験記録

| 項目                                 | 期待値                                     | 実測結果 |
| ------------------------------------ | ------------------------------------------ | -------- |
| ビルド SHA / バージョン              |                                            |          |
| Windows VM / OS build                | 専用・実データなし                         |          |
| 実施者 / UTC 日時                    |                                            |          |
| Service install/start/stop/uninstall | `BantoHub` が起動・停止し登録解除できる    |          |
| UaExpert endpoint / security         | endpoint に接続、Security None / Anonymous |          |
| Browse / Read / status               | 指定タグを発見、数値 Double、Good          |          |
| REST 更新 / DataChange               | 新値を通知、Good                           |          |
| SourceTimestamp                      | 値変更なしの連続 Read で同値               |          |
| UaExpert Write                       | 拒否、元値を保持                           |          |
| Service 再起動 / NodeId              | 同じ endpoint と NodeId で再接続           |          |
| 備考・ログ参照                       | 秘密値を含まない記録先                     |          |
