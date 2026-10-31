<#
  banto-hub 配布物の一括ビルド（I3、docs/banto-hub-installer-design.md
  §4.6・§6 row I3）。alpha.2 で実際に踏んだ手順（シェル・Hub 本体・elev・
  DB Sink サイドカーの4バイナリをビルド → 一体インストーラを生成）を
  1本にまとめ、リリース向けファイル名へのリネームと SHA256SUMS.txt の
  生成まで行う。CI からは呼ばない - Windows 実機で手動実行する配布物
  作成専用スクリプト（apps/banto-hub/installer/ 同様、ワークスペース外の
  xtask 的な立ち位置）。生成したインストーラを実行はしない（インストーラ
  生成ツール自身の方針と同じ - 実行してのインストール確認はオーナー
  判断領域）。
#>
[CmdletBinding()]
param(
    # 省略時はルート Cargo.toml の [workspace.package] version を使う。
    [string]$Version,
    # UI の dist が既にビルド済みで作り直し不要な場合にスキップ。
    [switch]$SkipFrontend,
    # exe 単体配布のみ欲しい場合（§7 決定9）にインストーラ生成を省く。
    [switch]$SkipInstaller,
    # 省略時は "dist/<version>"。
    [string]$OutDir
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot

# ネイティブコマンドは非0終了でも例外にならない（PowerShell の既知の罠）
# ため、$LASTEXITCODE を毎回明示的に確認して即座に止める。
function Invoke-Native {
    param([Parameter(Mandatory)][string]$Description, [Parameter(Mandatory)][scriptblock]$Command)
    Write-Host "==> $Description"
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Description が失敗しました（終了コード $LASTEXITCODE）"
    }
}

# --- バージョン決定 -------------------------------------------------------
if (-not $Version) {
    $cargoToml = Get-Content (Join-Path $repoRoot 'Cargo.toml') -Raw
    if ($cargoToml -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
        throw 'ルート Cargo.toml から [workspace.package] version を読み取れませんでした'
    }
    $Version = $Matches[1]
}
if (-not $OutDir) {
    $OutDir = Join-Path $repoRoot "dist\$Version"
}
# 後段の [System.IO.File]::WriteAllText はプロセスの作業フォルダ基準で相対パスを解決し、
# PowerShell の現在の場所（Copy-Item・Get-FileHash の基準）とずれることがある。また
# New-PSDrive で作ったドライブ（rel:\out など）は .NET が解決できない。相対指定も
# PSDrive も、PowerShell の現在の場所基準で実際のファイルシステムのパスにする。
$OutDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)
$releaseDir = Join-Path $repoRoot 'target\release'
Write-Host "banto-hub リリースビルド version=$Version OutDir=$OutDir"

# コピー先のリリース名の表（早い確かめと後段のコピーの両方で使う）。
$copies = [ordered]@{
    'banto-hub.exe'       = "banto-hub-v$Version-windows-x86_64.exe"
    'banto-hub-shell.exe' = "banto-hub-shell-v$Version-windows-x86_64.exe"
    'banto-hub-elev.exe'  = "banto-hub-elev-v$Version-windows-x86_64.exe"
    'banto-hub-sink.exe'  = "banto-hub-sink-v$Version-windows-x86_64.exe"
}
$cdlaName = 'CDLA-Permissive-2.0.txt'
$setupName = "BantoHub_${Version}_x64-setup.exe"
$planned = @($copies.Values) + @($cdlaName)
if (-not $SkipInstaller) { $planned += $setupName }
$planned += 'SHA256SUMS.txt'

# 前回の残りのファイルが $OutDir にあれば、長いビルドの前に失敗させる。ディレクトリ
# ごと配布すると SHA256SUMS.txt に載らないファイル（-SkipInstaller のときの前回の
# インストーラなど）まで配られるため。消しはしない。この実行で置く名前のファイルは
# 上書きされるので許す。
$plannedNames = [System.Collections.Generic.HashSet[string]]::new([string[]]$planned, [System.StringComparer]::OrdinalIgnoreCase)
if (Test-Path -LiteralPath $OutDir -PathType Container) {
    $leftover = @(Get-ChildItem -LiteralPath $OutDir -File | Where-Object { -not $plannedNames.Contains($_.Name) } | Sort-Object Name | ForEach-Object { $_.Name })
    if ($leftover.Count -gt 0) {
        throw ("$OutDir に、この実行で作らないファイルが残っています: " + ($leftover -join ', ') + "。SHA256SUMS.txt に載らないまま配られるのを避けるため中止します。消すか、別の -OutDir を指定してください。")
    }
}

# --- (a) ツールチェーン確認 ------------------------------------------------
Invoke-Native 'cargo --version' { cargo --version }
Invoke-Native 'pnpm --version' { pnpm --version }

Push-Location $repoRoot
try {
    # --- (b) フロントエンド（UI）ビルド ---
    if (-not $SkipFrontend) {
        Invoke-Native 'pnpm --filter banto-hub build' { pnpm --filter banto-hub build }
    }
    else {
        Write-Host '==> フロントエンドビルドをスキップ（-SkipFrontend）'
    }

    # --- (c)(d)(e) 4バイナリのリリースビルド（順序は installer/src/main.rs
    # の使い方コメントと同じ） ---
    Invoke-Native 'cargo build --release -p banto-hub-host --bin banto-hub --bin banto-hub-elev --features embed-ui' {
        cargo build --release -p banto-hub-host --bin banto-hub --bin banto-hub-elev --features embed-ui
    }
    Invoke-Native 'cargo build --release -p banto-hub-shell --features banto-hub-core/embed-ui' {
        cargo build --release -p banto-hub-shell --features banto-hub-core/embed-ui
    }
    Invoke-Native 'cargo build --release -p banto-hub-sink' {
        cargo build --release -p banto-hub-sink
    }

    # 4 exe の存在チェック（インストーラ生成ツール自身も同じ確認をするが、
    # ビルド漏れをここでも早期に検出する）。
    $requiredBinaries = @('banto-hub-shell', 'banto-hub', 'banto-hub-elev', 'banto-hub-sink')
    $missing = $requiredBinaries | Where-Object { -not (Test-Path (Join-Path $releaseDir "$_.exe")) }
    if ($missing) {
        throw "次の実行ファイルが見つかりません（$releaseDir）: $($missing -join ', ')"
    }

    # --- (f) 一体インストーラ生成 ---
    if (-not $SkipInstaller) {
        Invoke-Native 'cargo run --manifest-path apps/banto-hub/installer/Cargo.toml --release -- target/release' {
            cargo run --manifest-path (Join-Path $repoRoot 'apps\banto-hub\installer\Cargo.toml') --release -- $releaseDir
        }
    }
    else {
        Write-Host '==> インストーラ生成をスキップ（-SkipInstaller）'
    }
}
finally {
    Pop-Location
}

# --- (g) $OutDir へリリース名でコピー -------------------------------------
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# この実行で置いたファイル名（SHA256SUMS.txt に載せるのはこれだけ）。
$produced = [System.Collections.Generic.List[string]]::new()

foreach ($name in $copies.Keys) {
    Copy-Item -Path (Join-Path $releaseDir $name) -Destination (Join-Path $OutDir $copies[$name]) -Force
    $produced.Add($copies[$name])
}

# The bundled root certificate data uses CDLA-Permissive-2.0. Ship its full
# license text with standalone binaries as well as inside the NSIS installer.
$cdlaLicense = Join-Path $repoRoot 'apps\banto-hub\installer\assets\CDLA-Permissive-2.0.txt'
if (-not (Test-Path -LiteralPath $cdlaLicense -PathType Leaf)) {
    throw "Missing bundled certificate data license: $cdlaLicense"
}
Copy-Item -LiteralPath $cdlaLicense -Destination (Join-Path $OutDir $cdlaName) -Force
$produced.Add($cdlaName)

if (-not $SkipInstaller) {
    # tauri-bundler の既定命名規則（apps/banto-hub/installer/src/main.rs 参照）。
    $setupSrc = Join-Path $releaseDir "bundle\nsis\BantoHub_${Version}_x64-setup.exe"
    if (-not (Test-Path $setupSrc)) {
        throw "インストーラが見つかりません: $setupSrc"
    }
    Copy-Item -Path $setupSrc -Destination (Join-Path $OutDir $setupName) -Force
    $produced.Add($setupName)
}

# --- (h) SHA256SUMS.txt（sha256sum 互換の "<hash> *<name>" 形式） --------
# この実行で置いたファイルだけ。前回の残り（-SkipInstaller のときの setup.exe
# など）は、ビルドの前の確かめで失敗させてあるので載らない。
$sumsPath = Join-Path $OutDir 'SHA256SUMS.txt'
$lines = $produced | Sort-Object |
    ForEach-Object { '{0} *{1}' -f (Get-FileHash -Algorithm SHA256 -Path (Join-Path $OutDir $_)).Hash.ToLower(), $_ }
# Set-Content -Encoding utf8 は Windows PowerShell 5.1 だと BOM と CRLF を付け、
# sha256sum -c が 1 行目で失敗する。5.1 と 7 のどちらでも BOM なしの UTF-8・LF で
# 書く（sha256sum -c は CR を名前の一部と見る）。
[System.IO.File]::WriteAllText($sumsPath, (($lines -join "`n") + "`n"), [System.Text.UTF8Encoding]::new($false))

# --- (i) 一覧表示と注意喚起 -------------------------------------------------
Write-Host ''
Write-Host "生成物（$OutDir）:"
$produced | Sort-Object | ForEach-Object { Write-Host "  $_" }
Write-Host '  SHA256SUMS.txt'
Write-Host ''
Write-Host '注意: 生成したインストーラをビルド機の評価用 Hub に対して実行しないこと（Windows サービス化・ACL 付与が実環境に及ぶ - 実機確認は docs/banto-hub-installer-design.md §6 row I4 の手順で別途行う）。'
