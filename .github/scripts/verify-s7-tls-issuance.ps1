# Temporary #1006 verification on a disposable GitHub-hosted Windows runner.
[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [ValidateSet('default', 'alternate')]
  [string]$Store,

  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$OpenSslPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:OS -ne 'Windows_NT') {
  throw 'Run only on a disposable GitHub-hosted Windows runner.'
}
if ($OpenSslPath -notmatch '^[A-Za-z]:\\') {
  throw 'OpenSslPath must be a local drive absolute path.'
}
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [System.Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'An elevated Windows token is required.'
}

$programData = [Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)
$storePath = if ($Store -eq 'default') {
  Join-Path $programData 'BantoHub\tls'
} else {
  Join-Path $programData 'BantoHubTls'
}
if (Test-Path -LiteralPath $storePath) {
  throw "The fresh-runner test store must not exist: $storePath"
}
$target = Join-Path $PSScriptRoot '../../scripts/new-hub-loopback-tls.ps1'
$serviceSids = @('S-1-5-18', 'S-1-5-32-544')
$sidType = [System.Security.Principal.SecurityIdentifier]
$fullControl = [System.Security.AccessControl.FileSystemRights]::FullControl

function Confirm-ServiceAcl {
  param([string]$Path, [bool]$Directory)
  $item = Get-Item -LiteralPath $Path -Force
  if ($item.PSIsContainer -ne $Directory -or $item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
    throw "Unexpected output type: $Path"
  }
  $acl = Get-Acl -LiteralPath $Path
  $owner = $acl.GetOwner($sidType).Value
  if ($owner -notin $serviceSids -or ($Directory -and ($owner -ne 'S-1-5-32-544' -or -not $acl.AreAccessRulesProtected))) {
    throw "Unexpected owner or inheritance protection: $Path"
  }
  $rules = @($acl.GetAccessRules($true, $true, $sidType))
  foreach ($rule in $rules) {
    if ($rule.IdentityReference.Value -notin $serviceSids -or $rule.AccessControlType -ne 'Allow') {
      throw "Unexpected ACE: $Path"
    }
  }
  foreach ($sid in $serviceSids) {
    $full = @($rules | Where-Object {
      $_.IdentityReference.Value -eq $sid -and
      -not $_.PropagationFlags.HasFlag([System.Security.AccessControl.PropagationFlags]::InheritOnly) -and
      ($_.FileSystemRights -band $fullControl) -eq $fullControl
    })
    if ($full.Count -eq 0) {
      throw "FullControl is missing for $sid on $Path"
    }
  }
  Write-Host "ACL OK: $($item.Name), owner=$owner, protected=$($acl.AreAccessRulesProtected)"
}

$issued = & $target -TlsDirectory $storePath -Days 1 -OpenSslPath $OpenSslPath
Confirm-ServiceAcl $storePath $true
$outputs = @($issued.ConfigPath, $issued.PrivateKeyPath, $issued.CertificatePath)
$originalHashes = @{}
foreach ($path in $outputs) {
  if ([IO.Path]::GetDirectoryName($path) -ne $storePath -or $originalHashes.ContainsKey($path)) {
    throw 'The issuance script returned an unexpected artifact path.'
  }
  Confirm-ServiceAcl $path $false
  $originalHashes[$path] = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
}

$pem = [IO.File]::ReadAllText($issued.CertificatePath)
$der = [Convert]::FromBase64String($pem.Replace('-----BEGIN CERTIFICATE-----', '').Replace('-----END CERTIFICATE-----', ''))
$certificate = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($der)
$sha256 = [System.Security.Cryptography.SHA256]::Create()
try {
  $constraints = @($certificate.Extensions | Where-Object { $_.Oid.Value -eq '2.5.29.19' })
  $san = @($certificate.Extensions | Where-Object { $_.Oid.Value -eq '2.5.29.17' })
  # DER for a sequence containing exactly one IPv4 iPAddress, 127.0.0.1.
  if ($constraints.Count -ne 1 -or $constraints[0].CertificateAuthority -or -not $constraints[0].Critical -or
      $san.Count -ne 1 -or [BitConverter]::ToString($san[0].RawData) -ne '30-06-87-04-7F-00-00-01') {
    throw 'Certificate does not have critical CA:FALSE and IP SAN 127.0.0.1.'
  }
  $fingerprint = [BitConverter]::ToString($sha256.ComputeHash($certificate.RawData)).Replace('-', ':')
  $expiryStyle = [Globalization.DateTimeStyles]::AssumeUniversal -bor [Globalization.DateTimeStyles]::AdjustToUniversal
  $reportedExpiry = [DateTimeOffset]::ParseExact(
    ($issued.NotAfterUtc -replace '\s+', ' '),
    "MMM d HH:mm:ss yyyy 'GMT'",
    [Globalization.CultureInfo]::InvariantCulture,
    $expiryStyle
  ).UtcDateTime
  if ($fingerprint -cne $issued.FingerprintSha256 -or $reportedExpiry -ne $certificate.NotAfter.ToUniversalTime() -or
      $reportedExpiry -le [DateTime]::UtcNow -or $reportedExpiry -gt [DateTime]::UtcNow.AddDays(1)) {
    throw 'Certificate fingerprint or expiry does not match the issuance report.'
  }
} finally {
  $sha256.Dispose()
  $certificate.Dispose()
}
Write-Host "Issued: PowerShell=$($PSVersionTable.PSVersion), store=$Store, OpenSSL=$($issued.OpenSslVersion), CA:FALSE, IP SAN=127.0.0.1, fingerprint=$fingerprint, expiry=$($reportedExpiry.ToString('o'))"

function Confirm-OriginalOutputs {
  $current = @(Get-ChildItem -LiteralPath $storePath -Force)
  if ($current.Count -ne $originalHashes.Count) {
    throw 'The TLS store contains unexpected artifacts after the failure test.'
  }
  foreach ($item in $current) {
    if (-not $originalHashes.ContainsKey($item.FullName) -or
        (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash -cne $originalHashes[$item.FullName]) {
      throw "The failure test changed a successful artifact: $($item.Name)"
    }
  }
}
Confirm-OriginalOutputs

$source = [IO.File]::ReadAllText($target)
$anchor = '  $detailsText = Invoke-CheckedOpenSsl'
if ([regex]::Matches($source, [regex]::Escape($anchor)).Count -ne 1) {
  throw 'The failure injection point must be unique.'
}
$hook = @'
  $failureState.Artifacts = @($configFile, $keyFile, $certFile)
  if ($failureState.KeepCertificate) {
    $failureState.Lock = [IO.File]::Open($certFile, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
  }
  $null = Invoke-CheckedOpenSsl -Operation 'verification forced failure' -Arguments @('x509', '-in', "$certFile.missing", '-noout')

'@
$faultCopy = Join-Path $env:RUNNER_TEMP "s7-tls-fault-$([guid]::NewGuid().ToString('N')).ps1"
[IO.File]::WriteAllText($faultCopy, $source.Replace($anchor, $hook + $anchor), [Text.UTF8Encoding]::new($true))
try {
  foreach ($keepCertificate in @($false, $true)) {
    $failureState = @{ KeepCertificate = $keepCertificate; Lock = $null; Artifacts = @() }
    $failure = $null
    try {
      $null = & $faultCopy -TlsDirectory $storePath -Days 1 -OpenSslPath $OpenSslPath
    } catch {
      $failure = $_.Exception
    } finally {
      if ($null -ne $failureState.Lock) {
        $failureState.Lock.Dispose()
      }
    }
    if ($null -eq $failure -or $failure.Message -notmatch 'OpenSSL verification forced failure' -or $failureState.Artifacts.Count -ne 3) {
      throw "The expected OpenSSL failure was not observed: $failure"
    }

    $left = @($failureState.Artifacts | Where-Object { Test-Path -LiteralPath $_ })
    if ($keepCertificate) {
      $retained = $failureState.Artifacts[2]
      if ($left.Count -ne 1 -or $left[0] -ne $retained -or
          $null -eq $failure.InnerException -or $failure.InnerException.Message -notmatch 'OpenSSL verification forced failure' -or
          -not $failure.Message.Contains($retained)) {
        throw "Expected a single retained certificate and the original error: $failure"
      }
      Confirm-ServiceAcl $retained $false
      Remove-Item -LiteralPath $retained -ErrorAction Stop
    } elseif ($left.Count -ne 0) {
      throw 'The forced OpenSSL failure left generated artifacts behind.'
    }
    Confirm-OriginalOutputs
    Write-Host "Failure cleanup OK: retain-one test=$keepCertificate, original error preserved, prior output hashes unchanged"
  }
} finally {
  Remove-Item -LiteralPath $faultCopy -ErrorAction Stop
}

$issued
