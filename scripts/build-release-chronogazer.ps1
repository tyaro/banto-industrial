<#
  ChronoGazer 配布物の一括ビルド（#392 B3-4、docs/chronogazer-service-design.md
  §3.3「インストーラ」）。banto-hub の scripts/build-release.ps1 と同じ形:
  フロントエンド → シェル・記録エンジン（サービス）・昇格ヘルパーの 3 バイナリを
  リリースビルド → 一体インストーラ（apps/chronogazer/installer）を生成 →
  リリース向けファイル名へコピー → SHA256SUMS.txt。CI からは呼ばない - Windows
  実機で手動実行する配布物作成専用スクリプト。生成したインストーラを実行は
  しない（インストーラ生成ツール自身の方針と同じ - 実行してのインストール確認は
  オーナー同席の実機確認）。

  Hub のスクリプトとの違い: 環境変数 CARGO_TARGET_DIR があればそれを cargo の
  出力先として使う（worktree からビルドするときに main の checkout の target を
  共有できるように）。無ければリポジトリルートの target\release。
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
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repoRoot 'target' }
$releaseDir = Join-Path $targetDir 'release'
Write-Host "ChronoGazer リリースビルド version=$Version OutDir=$OutDir releaseDir=$releaseDir"

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
    # （embed-ui。シェルの LAN 公開と headless host の画面）。 ---
    Invoke-Native 'cargo build --release -p chronogazer --features embed-ui' {
        cargo build --release -p chronogazer --features embed-ui
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
}

# --- (g) $OutDir へリリース名でコピー -------------------------------------
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$copies = [ordered]@{
    'chronogazer.exe'        = "chronogazer-v$Version-windows-x86_64.exe"
    'chronogazer-server.exe' = "chronogazer-server-v$Version-windows-x86_64.exe"
    'chronogazer-elev.exe'   = "chronogazer-elev-v$Version-windows-x86_64.exe"
}
foreach ($name in $copies.Keys) {
    Copy-Item -Path (Join-Path $releaseDir $name) -Destination (Join-Path $OutDir $copies[$name]) -Force
}

if (-not $SkipInstaller) {
    # tauri-bundler の既定命名規則（apps/chronogazer/installer/src/main.rs 参照）。
    # 版は xtask の PRODUCT_VERSION（ルートの版に手で追従）で決まる。
    $setupSrc = Join-Path $releaseDir "bundle\nsis\ChronoGazer_${Version}_x64-setup.exe"
    if (-not (Test-Path $setupSrc)) {
        throw "インストーラが見つかりません: $setupSrc（apps/chronogazer/installer/src/main.rs の PRODUCT_VERSION がルートの版と合っているか確認）"
    }
    Copy-Item -Path $setupSrc -Destination (Join-Path $OutDir "ChronoGazer_${Version}_x64-setup.exe") -Force
}

# --- (h) SHA256SUMS.txt（sha256sum 互換の "<hash> *<name>" 形式） --------
$sumsPath = Join-Path $OutDir 'SHA256SUMS.txt'
$lines = Get-ChildItem -Path $OutDir -File |
    Where-Object { $_.Name -ne 'SHA256SUMS.txt' } |
    Sort-Object Name |
    ForEach-Object { '{0} *{1}' -f (Get-FileHash -Algorithm SHA256 -Path $_.FullName).Hash.ToLower(), $_.Name }
Set-Content -Path $sumsPath -Value $lines -Encoding utf8

# --- (i) 一覧表示と注意喚起 -------------------------------------------------
Write-Host ''
Write-Host "生成物（$OutDir）:"
Get-ChildItem -Path $OutDir -File | Sort-Object Name | ForEach-Object { Write-Host "  $($_.Name)" }
Write-Host ''
Write-Host '注意: 生成したインストーラをビルド機で実行しないこと（Windows サービス化・ACL 付与が実環境に及ぶ - 実機確認はオーナー同席で別途行う）。'
