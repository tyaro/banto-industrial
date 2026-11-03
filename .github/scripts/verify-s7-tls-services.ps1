# Disposable GitHub-hosted Windows job only. Exercises actual SCM services.
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$HubExe,
  [Parameter(Mandatory)][string]$SinkExe,
  [Parameter(Mandatory)][string]$ElevExe,
  [Parameter(Mandatory)][string]$CertificatePath,
  [Parameter(Mandatory)][string]$PrivateKeyPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $PSVersionTable.PSVersion.Major -lt 7) {
  throw 'Run only in PowerShell 7 on a disposable GitHub-hosted Windows runner.'
}
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'The service trial requires an elevated runner token.'
}
foreach ($path in @($HubExe, $SinkExe, $ElevExe, $CertificatePath, $PrivateKeyPath)) {
  if (-not [IO.Path]::IsPathFullyQualified($path) -or -not (Test-Path -LiteralPath $path -PathType Leaf)) {
    throw 'The service trial requires absolute paths to existing executables and PEM files.'
  }
}

function Invoke-TrialCommand {
  param([string]$Executable, [string]$Action)
  & $Executable $Action | Out-Host
  if ($LASTEXITCODE -ne 0) {
    throw "$([IO.Path]::GetFileName($Executable)) $Action failed with exit code $LASTEXITCODE."
  }
}

function Get-LoopbackPorts {
  $http = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
  $tls = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
  try {
    $http.Start()
    $tls.Start()
    return @($http.LocalEndpoint.Port, $tls.LocalEndpoint.Port)
  } finally {
    $tls.Stop()
    $http.Stop()
  }
}

function Remove-TrialService {
  param([string]$Name, [string]$Executable, [string]$Action)
  $service = Get-Service -Name $Name -ErrorAction SilentlyContinue
  if ($null -eq $service) { return }
  try {
    if ($service.Status -ne 'Stopped') {
      $service.Stop()
      $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60))
    }
  } finally {
    $service.Dispose()
  }
  Invoke-TrialCommand $Executable $Action
  $keyPath = Join-Path $serviceRegistry $Name
  $wait = [Diagnostics.Stopwatch]::StartNew()
  while ((Test-Path -LiteralPath $keyPath) -and $wait.Elapsed.TotalSeconds -lt 10) {
    Start-Sleep -Milliseconds 200
  }
  if (Test-Path -LiteralPath $keyPath) {
    throw "The trial service was not removed: $Name"
  }
}

function Remove-TrialTree {
  param([string]$Path, [string]$ExpectedParent)
  if (-not (Test-Path -LiteralPath $Path)) { return }
  $item = Get-Item -LiteralPath $Path -Force
  if (-not $item.PSIsContainer -or $item.FullName -ne $Path -or $item.Parent.FullName -ne $ExpectedParent -or
      $item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
    throw "Unexpected trial directory or link: $Path"
  }
  $links = @(Get-ChildItem -LiteralPath $item.FullName -Force -Recurse |
    Where-Object { $_.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) })
  if ($links.Count -ne 0) { throw "Link inside trial directory: $Path" }
  Remove-Item -LiteralPath $item.FullName -Recurse -Force
}

$programData = [Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)
$programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
$hubRoot = Join-Path $programData 'BantoHub'
$profiles = Join-Path $hubRoot 'profiles'
$sinkDir = Join-Path $hubRoot 'sink'
$sinkConfig = Join-Path $sinkDir 'banto-hub-sink.toml'
$serviceRegistry = 'HKLM:\SYSTEM\CurrentControlSet\Services'
$operators = 'BantoHub Operators'

foreach ($name in @('BantoHub', 'BantoHubSink')) {
  if ((Test-Path -LiteralPath (Join-Path $serviceRegistry $name)) -or
      (Get-Service -Name $name -ErrorAction SilentlyContinue)) {
    throw "The disposable runner already has a service named $name."
  }
}
if ((Test-Path -LiteralPath $profiles) -or (Test-Path -LiteralPath $sinkDir) -or
    (Test-Path -LiteralPath (Join-Path $hubRoot 'banto-hub-sink.toml')) -or
    (Get-LocalGroup -Name $operators -ErrorAction SilentlyContinue)) {
  throw 'The disposable runner already has a Hub profile, Sink configuration, or Operators group.'
}
if (-not (Test-Path -LiteralPath $hubRoot -PathType Container)) {
  throw 'Create the protected ProgramData\BantoHub parent before the service trial.'
}

$installDir = Join-Path $programFiles ('BantoHub-S7-TLS-' + [guid]::NewGuid().ToString('N'))
$hubInstalled = Join-Path $installDir 'banto-hub.exe'
$sinkInstalled = Join-Path $installDir 'banto-hub-sink.exe'
$elevInstalled = Join-Path $installDir 'banto-hub-elev.exe'
$example = Join-Path $PSScriptRoot '../../apps/banto-hub/installer/assets/banto-hub-sink.toml.example'
$hubAttempted = $false
$sinkAttempted = $false
$stageCreated = $false
$cleanupFailures = [Collections.Generic.List[string]]::new()
try {
  $null = New-Item -Path $installDir -ItemType Directory
  $stageCreated = $true
  Copy-Item -LiteralPath $HubExe -Destination $hubInstalled
  Copy-Item -LiteralPath $SinkExe -Destination $sinkInstalled
  Copy-Item -LiteralPath $ElevExe -Destination $elevInstalled
  Copy-Item -LiteralPath $example -Destination $installDir

  $hubAttempted = $true
  Invoke-TrialCommand $elevInstalled 'service-install'
  Invoke-TrialCommand $elevInstalled 'prepare-sink-config'

  $httpPort, $tlsPort = Get-LoopbackPorts
  $httpBase = "http://127.0.0.1:$httpPort"
  $serviceEnv = @(
    'BANTO_ALLOW_SETUP=1',
    'BANTO_BIND=127.0.0.1',
    "PORT=$httpPort",
    "BANTO_HUB_TLS_PORT=$tlsPort",
    "BANTO_HUB_TLS_CERT=$CertificatePath",
    "BANTO_HUB_TLS_KEY=$PrivateKeyPath"
  )
  $null = New-ItemProperty -LiteralPath (Join-Path $serviceRegistry 'BantoHub') -Name Environment -PropertyType MultiString -Value $serviceEnv
  Start-Service -Name BantoHub

  $csrf = @{ 'X-Banto-Client' = 'banto' }
  $setupStatus = $null
  $ready = [Diagnostics.Stopwatch]::StartNew()
  while ($ready.Elapsed.TotalSeconds -lt 45) {
    try {
      $setupStatus = Invoke-RestMethod -Uri "$httpBase/api/auth/status" -Headers $csrf -NoProxy -TimeoutSec 3
      break
    } catch {
      Start-Sleep -Milliseconds 500
    }
  }
  if ($null -eq $setupStatus -or $setupStatus.initialized -ne $false) {
    throw 'The Hub service did not expose an uninitialized test profile.'
  }

  $password = 'S7-tls-' + [guid]::NewGuid().ToString('N') + '!a9'
  try {
    $setup = @{
      Method = 'Post'
      Uri = "$httpBase/api/auth/setup"
      Headers = $csrf
      ContentType = 'application/json'
      Body = @{ username = 's7-tls-admin'; displayName = 'S7 TLS Test'; password = $password } | ConvertTo-Json -Compress
      NoProxy = $true
      TimeoutSec = 10
    }
    $auth = Invoke-RestMethod @setup
    if (-not $auth.success -or -not $auth.token) { throw 'Setup did not return a token.' }
    $issueKey = @{
      Method = 'Post'
      Uri = "$httpBase/api/api-keys"
      Headers = $csrf + @{ Authorization = "Bearer $($auth.token)" }
      ContentType = 'application/json'
      Body = @{ name = 's7-tls-sink'; scopes = @('admin', 'read') } | ConvertTo-Json -Compress
      NoProxy = $true
      TimeoutSec = 10
    }
    $key = Invoke-RestMethod @issueKey
    if (-not $key.key) { throw 'Key issuance did not return a key.' }
  } catch {
    throw 'The disposable Hub setup or API-key issuance failed.'
  }

  $api = @{ Authorization = "Bearer $($key.key)" }
  $baseline = Invoke-RestMethod -Uri "$httpBase/api/v1/status" -Headers $api -NoProxy -TimeoutSec 5
  if ($baseline.sink.sidecar.state -ne 'unknown' -or $null -ne $baseline.sink.sidecar.last_seen_at) {
    throw 'The Hub already has a Sink heartbeat before the trial service starts.'
  }
  $pem = [IO.File]::ReadAllText($CertificatePath).Trim()
  $config = @"
hub_url = "https://127.0.0.1:$tlsPort"
api_key = "$($key.key)"
hub_tls_pinned_certs = ['''$pem''']
status_push_secs = 1
"@
  [IO.File]::WriteAllText($sinkConfig, $config, [Text.UTF8Encoding]::new($false))
  Invoke-TrialCommand $elevInstalled 'prepare-sink-config'
  $sinkAttempted = $true
  Invoke-TrialCommand $sinkInstalled 'install'
  Start-Service -Name BantoHubSink

  $firstSeen = $null
  $advanced = $false
  $elapsed = [Diagnostics.Stopwatch]::StartNew()
  while ($elapsed.Elapsed.TotalSeconds -lt 45) {
    $status = Invoke-RestMethod -Uri "$httpBase/api/v1/status" -Headers $api -NoProxy -TimeoutSec 5
    $sidecar = $status.sink.sidecar
    if ($sidecar.state -eq 'online' -and $null -ne $sidecar.last_seen_at) {
      if ($null -eq $firstSeen) {
        $firstSeen = $sidecar.last_seen_at
      } elseif ($sidecar.last_seen_at -gt $firstSeen) {
        $advanced = $true
        break
      }
    }
    Start-Sleep -Milliseconds 500
  }
  $states = @(Get-Service -Name 'BantoHub', 'BantoHubSink' | ForEach-Object {
    try { $_.Status } finally { $_.Dispose() }
  })
  if (-not $advanced -or @($states | Where-Object { $_ -eq 'Running' }).Count -ne 2) {
    throw 'The two SCM services did not deliver advancing Sink heartbeats over the pinned TLS URL.'
  }
  Write-Host 'BantoHub and BantoHubSink running. Hub saw unknown -> online and advancing Sink heartbeats over pinned TLS.'
} finally {
  if ($sinkAttempted) {
    try { Remove-TrialService 'BantoHubSink' $sinkInstalled 'uninstall' }
    catch { $cleanupFailures.Add("BantoHubSink: $($_.Exception.Message)") }
  }
  if ($hubAttempted) {
    try { Remove-TrialService 'BantoHub' $elevInstalled 'service-uninstall' }
    catch { $cleanupFailures.Add("BantoHub: $($_.Exception.Message)") }
    try {
      if (Get-LocalGroup -Name $operators -ErrorAction SilentlyContinue) {
        Remove-LocalGroup -Name $operators
      }
    } catch { $cleanupFailures.Add("Operators: $($_.Exception.Message)") }
    if ($cleanupFailures.Count -eq 0) {
      foreach ($path in @($sinkDir, $profiles)) {
        try { Remove-TrialTree $path $hubRoot }
        catch { $cleanupFailures.Add($_.Exception.Message) }
      }
    }
  }
  if ($stageCreated -and $cleanupFailures.Count -eq 0) {
    try { Remove-TrialTree $installDir $programFiles }
    catch { $cleanupFailures.Add($_.Exception.Message) }
  }
  if ($cleanupFailures.Count -ne 0) {
    throw "Service trial cleanup failed: $($cleanupFailures -join ' | ')"
  }
  Write-Host 'Service trial cleanup complete.'
}
