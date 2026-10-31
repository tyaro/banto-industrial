<#
  ChronoGazer 配布物の一括ビルド（#392 B3-4、docs/chronogazer-service-design.md
  §3.3「インストーラ」）。banto-hub の scripts/build-release.ps1 と同じ形:
  フロントエンド → シェル・記録エンジン（サービス）・昇格ヘルパーの 3 バイナリを
  リリースビルド → 一体インストーラ（apps/chronogazer/installer）を生成 →
  リリース向けファイル名へコピー → SHA256SUMS.txt。CI からは呼ばない - Windows
  実機で手動実行する配布物作成専用スクリプト。生成したインストーラを実行は
  しない（インストーラ生成ツール自身の方針と同じ - 実行してのインストール確認は
  オーナー同席の実機確認）。

  cargo の出力先は常にリポジトリルートの target-release（これらのスクリプトだけが使う。
  worktree が CARGO_TARGET_DIR に向ける共有の target ではない）。継承した環境変数
  CARGO_TARGET_DIR は警告して無視する（2026-10-31、B3-5: 共有の target に別の
  worktree が置いた exe が、ビルドと梱包の間に上書きされ得る。時刻では出どころを
  証明できない）。この変更後の最初のビルドはフルビルドになる。
  SHA256SUMS.txt と最後の一覧は、この実行でコピーしたファイルだけから作る
  （-SkipInstaller で既存の OutDir に重ねると、前回の setup.exe が残ったまま
  一覧と検査和に載る、を防ぐ。#972 の Codex の指摘）。残っているほかの
  ファイルは消さずに警告する。
#>
[CmdletBinding()]
param(
    # 省略時はルート Cargo.toml の [workspace.package] version を使う。
    [string]$Version,
    # UI の build が既にビルド済みで作り直し不要な場合にスキップ。
    [switch]$SkipFrontend,
    # exe 単体配布のみ欲しい場合にインストーラ生成を省く。
    [switch]$SkipInstaller,
    # 省略時は "dist/chronogazer/<version>"。
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
    $OutDir = Join-Path $repoRoot "dist\chronogazer\$Version"
}
# cargo は $repoRoot で走る（Push-Location）ので相対の CARGO_TARGET_DIR は $repoRoot 基準、
# 後段の Copy-Item は Pop-Location 後の呼び出し元の cwd 基準になってずれる。ビルドの
# 前に絶対パスへ解決する。$OutDir の相対指定も、New-PSDrive で作ったドライブ
# （rel:\out など。.NET は解決できない）も、PowerShell の現在の場所基準で実際の
# ファイルシステムのパスにする。
$OutDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)
$ownTarget = [System.IO.Path]::GetFullPath((Join-Path $repoRoot 'target-release'))
$prevCargoTargetDir = $env:CARGO_TARGET_DIR
if ($prevCargoTargetDir -and ([System.IO.Path]::GetFullPath((Join-Path $repoRoot $prevCargoTargetDir)).TrimEnd('\') -ne $ownTarget.TrimEnd('\'))) {
    Write-Warning "環境変数 CARGO_TARGET_DIR（$prevCargoTargetDir）はリリースビルドでは無視し、専用の $ownTarget に作る。"
}
$env:CARGO_TARGET_DIR = $ownTarget
$targetDir = $ownTarget
$releaseDir = Join-Path $targetDir 'release'
Write-Host "ChronoGazer リリースビルド version=$Version OutDir=$OutDir releaseDir=$releaseDir"

# コピー先のリリース名の表（早い確かめと後段のコピーの両方で使う）。
$copies = [ordered]@{
    'chronogazer.exe'        = "chronogazer-v$Version-windows-x86_64.exe"
    'chronogazer-server.exe' = "chronogazer-server-v$Version-windows-x86_64.exe"
    'chronogazer-elev.exe'   = "chronogazer-elev-v$Version-windows-x86_64.exe"
}
$setupName = "ChronoGazer_${Version}_x64-setup.exe"
$planned = @($copies.Values)
if (-not $SkipInstaller) { $planned += $setupName }
$planned += 'SHA256SUMS.txt'

# 前回の残りのファイルが $OutDir にあれば、長いビルドの前に失敗させる。ディレクトリ
# ごと配布すると SHA256SUMS.txt に載らないファイル（-SkipInstaller のときの前回の
# インストーラなど）まで配られるため。消しはしない。この実行で置く名前のファイルは
# 上書きされるので許す。
$plannedNames = [System.Collections.Generic.HashSet[string]]::new([string[]]$planned, [System.StringComparer]::OrdinalIgnoreCase)
if (Test-Path -LiteralPath $OutDir -PathType Container) {
    $leftover = @(Get-ChildItem -LiteralPath $OutDir -Force | Where-Object { -not $plannedNames.Contains($_.Name) } | Sort-Object Name | ForEach-Object { $_.Name })
    if ($leftover.Count -gt 0) {
        throw ("$OutDir に、この実行で作らないファイルが残っています: " + ($leftover -join ', ') + "。SHA256SUMS.txt に載らないまま配られるのを避けるため中止します。消すか、別の -OutDir を指定してください。")
    }
}

# --- (a) ツールチェーン確認 ------------------------------------------------
Invoke-Native 'cargo --version' { cargo --version }
Invoke-Native 'pnpm --version' { pnpm --version }

Push-Location $repoRoot
try {
    # --- (b) フロントエンド（UI）ビルド（apps/chronogazer/package.json の build = vite build） ---
    if (-not $SkipFrontend) {
        Invoke-Native 'pnpm --filter chronogazer build' { pnpm --filter chronogazer build }
    }
    else {
        Write-Host '==> フロントエンドビルドをスキップ（-SkipFrontend）'
    }

    # --- (c)(d)(e) 3 バイナリのリリースビルド（順序は installer/src/main.rs
    # の使い方コメントと同じ）。シェルと記録エンジンは UI を埋め込む
    # （embed-ui。シェルの LAN 公開と headless host の画面）。
    # シェルには tauri/custom-protocol も付ける。これが無いと tauri は開発の
    # ビルドとみなし、窓が同梱の UI ではなく devUrl（http://localhost:1420）を
    # 開く（`cargo tauri build` は自分で付けるが、ここは cargo を直に呼ぶ）。 ---
    Invoke-Native 'cargo build --release -p chronogazer --features embed-ui,tauri/custom-protocol' {
        cargo build --release -p chronogazer --features embed-ui,tauri/custom-protocol
    }
    Invoke-Native 'cargo build --release -p chronogazer-core --bin chronogazer-server --features embed-ui' {
        cargo build --release -p chronogazer-core --bin chronogazer-server --features embed-ui
    }
    Invoke-Native 'cargo build --release -p chronogazer-elev' {
        cargo build --release -p chronogazer-elev
    }

    # 3 exe の存在チェック（インストーラ生成ツール自身も同じ確認をするが、
    # ビルド漏れをここでも早期に検出する）。
    $requiredBinaries = @('chronogazer', 'chronogazer-server', 'chronogazer-elev')
    $missing = $requiredBinaries | Where-Object { -not (Test-Path (Join-Path $releaseDir "$_.exe")) }
    if ($missing) {
        throw "次の実行ファイルが見つかりません（$releaseDir）: $($missing -join ', ')"
    }

    # --- (f) 一体インストーラ生成（xtask は独立したワークスペースで、自分の
    # target（apps/chronogazer/installer/target）に出る。Hub と同じ） ---
    if (-not $SkipInstaller) {
        Invoke-Native 'cargo run --manifest-path apps/chronogazer/installer/Cargo.toml --release -- <releaseDir>' {
            cargo run --manifest-path (Join-Path $repoRoot 'apps\chronogazer\installer\Cargo.toml') --release -- $releaseDir
        }
    }
    else {
        Write-Host '==> インストーラ生成をスキップ（-SkipInstaller）'
    }
}
finally {
    Pop-Location
    $env:CARGO_TARGET_DIR = $prevCargoTargetDir
}

# --- (g) $OutDir へリリース名でコピー -------------------------------------
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# この実行で $OutDir に置いたファイル名（SHA256SUMS.txt と一覧はこれだけから作る）。
$produced = [System.Collections.Generic.List[string]]::new()

foreach ($name in $copies.Keys) {
    Copy-Item -Path (Join-Path $releaseDir $name) -Destination (Join-Path $OutDir $copies[$name]) -Force
    $produced.Add($copies[$name])
}

if (-not $SkipInstaller) {
    # tauri-bundler の既定命名規則（apps/chronogazer/installer/src/main.rs 参照）。
    # 版は xtask の PRODUCT_VERSION（ルートの版に手で追従）で決まる。
    $setupSrc = Join-Path $releaseDir "bundle\nsis\ChronoGazer_${Version}_x64-setup.exe"
    if (-not (Test-Path $setupSrc)) {
        throw "インストーラが見つかりません: $setupSrc（apps/chronogazer/installer/src/main.rs の PRODUCT_VERSION がルートの版と合っているか確認）"
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
# Set-Content -Encoding utf8 は Windows PowerShell 5.1 だと BOM を付け、sha256sum が
# 1 行目のハッシュを読めなくなる（#972 のオーナーレビュー（7f86c38））。5.1 と 7 の
# どちらでも BOM なしの UTF-8 で書く。改行は LF（sha256sum -c は CR を名前の一部と
# 見るため）。
[System.IO.File]::WriteAllText($sumsPath, (($lines -join "`n") + "`n"), [System.Text.UTF8Encoding]::new($false))

# --- (i) 一覧表示と注意喚起 -------------------------------------------------
Write-Host ''
Write-Host "生成物（$OutDir）:"
$produced | Sort-Object | ForEach-Object { Write-Host "  $_" }
Write-Host '  SHA256SUMS.txt'
Write-Host ''
Write-Host '注意: 生成したインストーラをビルド機で実行しないこと（Windows サービス化・ACL 付与が実環境に及ぶ - 実機確認はオーナー同席で別途行う）。'
