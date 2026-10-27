# #229 S7 実 DB 検証の記録（外部 DB 連携、実機・別マシンの PostgreSQL）

作成日: 2026-10-28
状態: **未実施**（2026-10-28: 準備 P-7 として空の表と R-2 の採取スクリプトを用意した。スクリプトは同日、ローカルの PostgreSQL 15（`127.0.0.1:5432`）と模擬の Hub で PowerShell 7（3 行）・5.1（2 行）を流し、件数・増分・CSV の形と、Hub 不在・psql の接続拒否・キー未設定でも止まらないことを確かめた。本物の Hub での採取は当日の機材で 1 回試す）

[external-db-test-2026-09.md](external-db-test-2026-09.md) §3.3 の S7 本番（2026-11-02〜11-08、オーナー同席）の結果を書く文書。**手順と期待は手順書が正で、この文書は記録だけ**（各段の順番は §3.3 D、期待は §2・§3・§3.1・§3.3 S）。値は当日に埋める。キー・パスワードは書かない。

## 1. 機材と版（P-1・P-2）

### P-1 機材・LAN・切断の手段

| 項目                             | 記録                                                                                                                                                                                                                                                                            |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Hub・サイドカーの PC（名前・IP） | `MSI-A13`。DB への経路は Wi-Fi の 192.168.10.9（PLC は有線の 192.168.11.137 側で、DB の経路と分かれている）                                                                                                                                                                     |
| Hub・サイドカーの PC の OS       | Windows 11 Pro 10.0.26300                                                                                                                                                                                                                                                       |
| PostgreSQL の PC（名前・IP）     | Mac（`KentaronoMacBook-Air`）、192.168.10.12（Wi-Fi）                                                                                                                                                                                                                           |
| PostgreSQL の PC の OS           | macOS 27.2。PostgreSQL は Homebrew の `postgresql@15`（設定は `/opt/homebrew/var/postgresql@15/`）                                                                                                                                                                              |
| PostgreSQL の版                  | PostgreSQL 15.19 (Homebrew) on aarch64-apple-darwin27.0.0（2026-10-28 に `SELECT version()`）                                                                                                                                                                                   |
| LAN の経路                       | 2 台とも同じ Wi-Fi の 192.168.10.0/24（Hub の PC から見て直結の経路、ルーターを挟まない）。2026-10-28 に Hub の PC から 5432 の TCP が通ることを確かめた（ping の応答は無い）                                                                                                   |
| 切断の手段                       | **Mac の Wi-Fi を切る**（2026-10-28 のオーナー決定。DB の PC が Wi-Fi で繋がるのでケーブルの案から替えた）                                                                                                                                                                      |
| NTP のずれの実測                 | 2026-10-28 02:40（JST）に Hub の PC から `psql -c "SELECT now()"` を 2 回流し、返った直後の PC の時計と比べた: DB 02:40:40.242 / PC 02:40:40.247、DB 02:40:44.545 / PC 02:40:44.536。DB の時計が Hub の PC より約 9 ms 以上進む（往復を含む粗い測り。秒の単位の照合には足りる） |

### P-2 版

| 項目                               | 記録                                                                                                                                                              |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Hub のコミット                     | `61d69378`（main、#891）。2026-10-28 に `scripts/build-release.ps1` で release ビルド                                                                             |
| `banto-hub-sink.exe` のコミット    | `61d69378`（同じビルド）。SHA256 `66e4d873…`                                                                                                                      |
| `banto-hub-elev` のコミット        | `61d69378`（同じビルド）。SHA256 `e14da465…`                                                                                                                      |
| インストーラの版・ファイル名       | `BantoHub_0.3.0-alpha.1_x64-setup.exe`（0.3.0-alpha.1）。SHA256 `64083c8960402d21aea1a8337b1675e105bcaacd94210c9b0ee220b126dc7a54`。Hub 本体の SHA256 `12548412…` |
| 当日途中の入れ替え（無いのが期待） |                                                                                                                                                                   |

### P-3〜P-6 準備の記録

| 項目                 | 実施日     | 結果                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| -------------------- | ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P-3 DB とユーザー    | 2026-10-28 | データベース `banto_s7` に §1 の 2 つのテーブルとユーザー `banto_source`・`banto_sink` を作り、手順書の「最小の権限の SQL」を流した（オーナーが Mac で実施）。`\dp machine_status` は `banto_source=r/postgres`、`\dp tag_history` は `banto_sink=ar/postgres`、`\dn+ public` は両ユーザーとも `U` だけ（`PUBLIC` も `U` だけで `C` が無い）。`SET ROLE banto_sink; CREATE TABLE t_probe(i int);` は `permission denied for schema public` で断られた。合格。パスワードは一時的なもの（オーナー判断）で、記録しない                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| P-4 接続の制限とログ | 2026-10-28 | `pg_hba.conf` の既存の `host all all 192.168.10.0/24 scram-sha-256` の前に `host banto_s7 banto_source,banto_sink 192.168.10.9/32 scram-sha-256` と `host all banto_source,banto_sink 0.0.0.0/0 reject` を足した（手順の重複で同じ 2 行が 2 組入ったが、先の組が当たるので動きは同じ。`pg_hba_file_rules` の `error` は空）。`ALTER SYSTEM` で `log_connections`・`log_disconnections` を on、`log_statement = 'ddl'`、`log_line_prefix = '%m [%p] %u@%d %a '` にして `pg_reload_conf()`。Hub の PC から `psql` で 2 つのユーザーが繋がった。Mac 自身の LAN 側の IP（192.168.10.12）から `banto_sink` で繋ぐと断られることをオーナーが確かめた（ほかの PC から断られることの代わり）。**ログに `application_name` が出ることの確かめは省略**（オーナー判断）。これは D-1 で Hub の接続のログを見て確かめる。ログは `/opt/homebrew/var/log/postgresql@15.log`。S7 の後に戻すには、`banto_source,banto_sink` を含む行を消して `pg_reload_conf()`（`pg_hba.conf.bak-s7` は 2 回目の `cp` で上書きされ、元の写しではない） |
| P-5 Hub の専用 TLS   |            |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| P-6 API キーの手順   |            |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |

## 2. 記録

### R-1 項目の合否（D・S・A・B・C のすべての項目で 1 行）

| 項目 | 実施の時刻 | 合否 | 期待と違ったこと | 機材の癖か製品の問題か | 関連のログ・Issue |
| ---- | ---------- | ---- | ---------------- | ---------------------- | ----------------- |
|      |            |      |                  |                        |                   |

### R-2 24 時間の 1 時間ごとの採取（D-3）

付録のスクリプトが 1 時間ごとに CSV へ足した行を貼る（または CSV を添付する）。`/api/v1/status` のフィールドは snake_case。列は手順書 §3.3 R-2 の表と同じ項目で、Sink のグループ・DB Source の接続・取り込み口が複数ある列は 1 つのセルに `名前=値;` を連ねる。

| 時刻 | Sink の `state` | `queued` | `dropped` | `last_flush_at` | `last_error` | DB Source の状態・`restarts` | 取り込み口の `accepted`・`rejected`・`rate_limited`・`too_many_items` | `tag_history` の行数 | 直前の 1 時間の増分（期待は 6 タグ × 3600 = 21,600。事象の時間は減る） |
| ---- | --------------- | -------- | --------- | --------------- | ------------ | ---------------------------- | --------------------------------------------------------------------- | -------------------- | ---------------------------------------------------------------------- |
|      |                 |          |           |                 |              |                              |                                                                       |                      |                                                                        |

### R-3 欠落と重複の照合（D-3 の後に 1 回）

SQL は手順書 §3.3 R-3。結果は**候補の数**として書き、R-4 の事象で説明できないものを別に数える（この記録だけでは本当の欠落・再送による重複を確定しない。確定できないときはそう書く）。

| 項目                                             | 記録 |
| ------------------------------------------------ | ---- |
| `<開始>`・`<終了>`                               |      |
| 欠落の候補（2.5 秒を超えた間隔）の数             |      |
| 重複の候補（同じタグ・`ts`・値の Good の行）の数 |      |
| R-4 の事象で説明できない候補の数                 |      |

欠落の SQL の結果:

```text

```

重複の SQL の結果:

```text

```

### R-4 事象と復帰（S-1・S-3・B-21 など、止めたり切ったりした項目ごとに 1 行）

| 事象 | 開始の時刻 | 戻した時刻 | DB Source が Good に戻った時刻 | Sink の行が再開した時刻 | `queued` の最大 | `dropped` の増分 | Hub の収集の途切れ（無いのが期待） | 欠落・重複の候補の数（R-3、確定できない場合はそう書く） |
| ---- | ---------- | ---------- | ------------------------------ | ----------------------- | --------------- | ---------------- | ---------------------------------- | ------------------------------------------------------- |
|      |            |            |                                |                         |                 |                  |                                    |                                                         |

### R-5 欠ける範囲（S-4）

| 操作（rename・削除→作り直し） | 操作の時刻 | タグ（作り直しは前後の `tag_id`） | 最後の行の `ts` | 再開した最初の行の `ts` | 欠けた秒数 | ほかのタグの欠落 |
| ----------------------------- | ---------- | --------------------------------- | --------------- | ----------------------- | ---------- | ---------------- |
|                               |            |                                   |                 |                         |            |                  |

### R-6 Stale の観測（B-22・B-23・S-1 で Bad や Good の前に Stale を見たとき）

| 観測の時刻（ミリ秒） | タグ | 保存されている品質の並び | 直前の収集の停止・開始の時刻 | DB の問い合わせの間隔 |
| -------------------- | ---- | ------------------------ | ---------------------------- | --------------------- |
|                      |      |                          |                              |                       |

## 付録 R-2 の採取スクリプト

PowerShell 7 と Windows PowerShell 5.1 のどちらでも動く。Hub の `GET /api/v1/status`（`Authorization: Bearer <read スコープの API キー>`）と `psql` の `SELECT count(*), max(ts) FROM tag_history` を 1 回ごとに CSV へ 1 行足す。API か psql の片方が失敗しても止まらず、その列を空にして `error` 列に短い理由を書き、次の回へ進む。`last_flush_at` はローカル時刻の ISO 8601 に直す。`delta_since_prev` は直前に取れた件数との差（psql が続けて失敗したときは、その間の合計の増分になる）。psql は `-w` でパスワードを対話で求めず、求められたら失敗として `error` 列に書く（夜間に入力待ちで止まらないため）。

**キーは R-2 の採取用に `read` だけのものを 1 つ発行する**（`admin` を付けない。Sink のキーを流用しない。手順書 P-6）。

文字コードの取り違えを避けるため、スクリプトは ASCII だけで書いてある（5.1 は BOM の無いファイルを ANSI として読む）。`s7-collect-r2.ps1` として保存して使う。リポジトリには置かない。

```powershell
<#
  S7 R-2 hourly sampler. Appends one CSV row per round: GET <HubUrl>/api/v1/status + count/max(ts) of tag_history.
  API key: env BANTO_S7_READ_KEY (read scope). DB password: env PGPASSWORD.
  Neither is written to this script, the CSV or the console. Stop with Ctrl+C.
#>
[CmdletBinding()]
param(
    [string]$HubUrl = 'http://127.0.0.1:8722',
    [string]$CsvPath = '.\s7-r2.csv',
    [int]$IntervalSeconds = 3600,
    [int]$Count = 0,                 # 0 = run until stopped
    [string]$PsqlPath = 'psql',
    [string]$PgHost, [string]$PgPort, [string]$PgUser, [string]$PgDatabase,  # if omitted, PG* env vars apply
    [string]$KeyEnvName = 'BANTO_S7_READ_KEY'
)

$ErrorActionPreference = 'Continue'
$csv = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($CsvPath)
$utf8 = New-Object System.Text.UTF8Encoding($false)
$columns = @('time', 'sink_state', 'sink_queued', 'sink_dropped', 'sink_last_flush_at', 'sink_last_error',
    'db_source_state', 'db_source_restarts', 'ingest_accepted', 'ingest_rejected', 'ingest_rate_limited',
    'ingest_too_many_items', 'tag_history_count', 'tag_history_max_ts', 'delta_since_prev', 'error')

function ConvertTo-Cell([object]$Value) {
    if ($null -eq $Value) { return '' }
    $s = [string]$Value
    if ($s -match '[",\r\n]') { return '"' + ($s -replace '"', '""') + '"' }
    return $s
}
# Join array items as "name=value;" into one cell.
function Join-Items($Items, [scriptblock]$Name, [scriptblock]$Val) {
    $out = ''
    foreach ($i in @($Items)) {
        $v = & $Val $i
        if ($null -eq $v) { $v = '' }
        $out += ('{0}={1};' -f (& $Name $i), $v)
    }
    return $out
}
function Format-Ms($Ms) {
    if ($null -eq $Ms) { return '' }
    return ([DateTimeOffset]::FromUnixTimeMilliseconds([int64]$Ms)).ToLocalTime().ToString('yyyy-MM-ddTHH:mm:sszzz')
}
function Short([string]$Text) {
    $t = ($Text -replace '\s+', ' ').Trim()
    if ($t.Length -gt 120) { $t = $t.Substring(0, 120) }
    return $t
}

if (-not (Test-Path -LiteralPath $csv) -or (Get-Item -LiteralPath $csv).Length -eq 0) {
    [System.IO.File]::AppendAllText($csv, ($columns -join ',') + "`r`n", $utf8)
}
$env:PGCONNECT_TIMEOUT = '10'
$prevCount = $null
$n = 0
while ($true) {
    $started = Get-Date
    $row = [ordered]@{}
    foreach ($c in $columns) { $row[$c] = '' }
    $row['time'] = $started.ToString('yyyy-MM-ddTHH:mm:sszzz')
    $errors = @()

    # --- Hub status ---
    try {
        $key = [Environment]::GetEnvironmentVariable($KeyEnvName)
        if ([string]::IsNullOrEmpty($key)) { throw "env $KeyEnvName is not set" }
        $st = Invoke-RestMethod -Uri ($HubUrl.TrimEnd('/') + '/api/v1/status') -Method Get -TimeoutSec 15 `
            -Headers @{ Authorization = "Bearer $key" }
        $g = $st.sink.groups
        $row['sink_state'] = Join-Items $g { param($x) $x.id } { param($x) $x.state }
        $row['sink_queued'] = Join-Items $g { param($x) $x.id } { param($x) $x.queued }
        $row['sink_dropped'] = Join-Items $g { param($x) $x.id } { param($x) $x.dropped }
        $row['sink_last_flush_at'] = Join-Items $g { param($x) $x.id } { param($x) Format-Ms $x.last_flush_at }
        $row['sink_last_error'] = Join-Items $g { param($x) $x.id } { param($x) Short ([string]$x.last_error) }
        $d = $st.db_source
        $row['db_source_state'] = Join-Items $d { param($x) $x.connection_name } { param($x) $x.state }
        $row['db_source_restarts'] = Join-Items $d { param($x) $x.connection_name } { param($x) $x.restarts }
        $ig = $st.ingest
        $row['ingest_accepted'] = Join-Items $ig { param($x) $x.name } { param($x) $x.accepted }
        $row['ingest_rejected'] = Join-Items $ig { param($x) $x.name } { param($x) $x.rejected }
        $row['ingest_rate_limited'] = Join-Items $ig { param($x) $x.name } { param($x) $x.rate_limited }
        $row['ingest_too_many_items'] = Join-Items $ig { param($x) $x.name } { param($x) $x.too_many_items }
    } catch {
        $errors += ('api: ' + (Short $_.Exception.Message))
    }

    # --- DB row count ---
    try {
        $psqlArgs = @('-X', '-q', '-w', '-t', '-A', '-F', ',')   # -w: never prompt for a password (fail instead)
        if ($PgHost) { $psqlArgs += @('-h', $PgHost) }
        if ($PgPort) { $psqlArgs += @('-p', $PgPort) }
        if ($PgUser) { $psqlArgs += @('-U', $PgUser) }
        if ($PgDatabase) { $psqlArgs += @('-d', $PgDatabase) }
        $psqlArgs += @('-c', 'SELECT count(*), max(ts) FROM tag_history')
        $out = & $PsqlPath @psqlArgs 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw (($out | ForEach-Object { [string]$_ }) -join ' ')
        }
        $last = @($out | Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] } |
                ForEach-Object { [string]$_ } | Where-Object { $_.Trim() -ne '' }) | Select-Object -Last 1
        $parts = ([string]$last).Split(',', 2)
        $cnt = 0L
        if ($parts.Count -lt 2 -or -not [int64]::TryParse($parts[0], [ref]$cnt)) { throw "unexpected psql output: $last" }
        $row['tag_history_count'] = $cnt
        $row['tag_history_max_ts'] = $parts[1]
        if ($null -ne $prevCount) { $row['delta_since_prev'] = $cnt - $prevCount }
        $prevCount = $cnt
    } catch {
        $errors += ('psql: ' + (Short $_.Exception.Message))
    }

    $row['error'] = $errors -join ' | '
    $line = (($columns | ForEach-Object { ConvertTo-Cell $row[$_] }) -join ',')
    try { [System.IO.File]::AppendAllText($csv, $line + "`r`n", $utf8) }
    catch { Write-Warning ('cannot write CSV: ' + $_.Exception.Message) }
    Write-Host ($row['time'] + '  count=' + $row['tag_history_count'] + '  error=' + $row['error'])

    $n++
    if ($Count -gt 0 -and $n -ge $Count) { break }
    $wait = $IntervalSeconds - ((Get-Date) - $started).TotalSeconds
    if ($wait -gt 0) { Start-Sleep -Seconds ([int][math]::Ceiling($wait)) }
}
```

### 使い方

```powershell
# キーとパスワードは環境変数で渡す（スクリプト・CSV・画面には出ない）
$env:BANTO_S7_READ_KEY = '<read スコープのキー>'
$env:PGPASSWORD = '<DB のパスワード>'

# 24 時間（1 時間ごと、25 行）。既定の間隔は 3600 秒
.\s7-collect-r2.ps1 -HubUrl http://127.0.0.1:8722 -CsvPath .\s7-r2.csv -Count 25 `
  -PsqlPath 'C:\Program Files\PostgreSQL\15\bin\psql.exe' `
  -PgHost <PostgreSQL の PC の IP> -PgUser <ユーザー> -PgDatabase <DB>

# 形の確認（5 秒ごとに 3 行）
.\s7-collect-r2.ps1 -IntervalSeconds 5 -Count 3 -CsvPath .\try.csv
```

- 止め方: `-Count` を省略すると止めるまで続く。止めるのは Ctrl+C（CSV は 1 回ごとに追記しているので、止めても失われない）。
- `-PgHost` などを省略すると `PGHOST`・`PGPORT`・`PGUSER`・`PGDATABASE` の環境変数が効く。接続の待ちは `PGCONNECT_TIMEOUT` = 10 秒にしてある。
- パスワードは `PGPASSWORD` だけで渡す。`-PgDatabase` に `postgresql://user:pass@…` の接続 URI を書かない（`error` 列やプロセスの一覧に出得る）。
- 実行ポリシーで `.ps1` が動かない PC では `powershell -ExecutionPolicy Bypass -File .\s7-collect-r2.ps1 …`（7 なら `pwsh`）で起動する。
- `tag_history_max_ts` は psql のセッションの `TimeZone`（`PGTZ` か DB の既定）で表示される。`time` 列（PC のローカル時刻）と時刻帯が違って見えたらそのため。R-3 の SQL の `<開始>`・`<終了>` も時刻帯を付けて書く。
- Hub の URL の既定は `http://127.0.0.1:8722`。`/api/v1/status` は管理系の `GET /api/status`（ログインのセッションが要る）とは別で、API キー（`read`）で読める。
- 2 回目以降は前の回の終わりではなく開始から `-IntervalSeconds` 後に次の回を始める。夜をまたぐ間は、スクリプトを動かす PC がスリープしないようにしておく。
- CSV は UTF-8（BOM なし）。ヘッダーは新しいファイルの最初だけ。同じファイルへ再開して足してもよい（`delta_since_prev` は再開の 1 行目だけ空になる）。

CSV の列:

| 列                                                                                   | 内容                                                                                                                             |
| ------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| `time`                                                                               | 採取の開始時刻（ローカル、ISO 8601）                                                                                             |
| `sink_state`・`sink_queued`・`sink_dropped`・`sink_last_flush_at`・`sink_last_error` | `sink.groups[]` の `state`・`queued`・`dropped`・`last_flush_at`・`last_error`。グループの `id` ごとに `1=running;2=error;` の形 |
| `db_source_state`・`db_source_restarts`                                              | `db_source[]` の `state`・`restarts`。接続名ごとに `名前=値;`                                                                    |
| `ingest_accepted`・`ingest_rejected`・`ingest_rate_limited`・`ingest_too_many_items` | `ingest[]` の `accepted`・`rejected`・`rate_limited`・`too_many_items`。接続名ごとに `名前=値;`                                  |
| `tag_history_count`・`tag_history_max_ts`                                            | `SELECT count(*), max(ts) FROM tag_history` の結果                                                                               |
| `delta_since_prev`                                                                   | 直前に取れた `tag_history_count` からの増分（最初の行は空）                                                                      |
| `error`                                                                              | `api: …`・`psql: …` の短い理由（120 文字まで）。成功した回は空                                                                   |
