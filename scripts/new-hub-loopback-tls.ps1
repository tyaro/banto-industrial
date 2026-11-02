# Hub–Sink 専用 loopback TLS の自己署名サーバー証明書を発行する。
# サービス環境変数、Sink の pin、サービスの起動は変更しない。
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$TlsDirectory,

  [Parameter(Mandatory = $true)]
  [ValidateRange(1, 36500)]
  [int]$Days,

  [string]$OpenSslPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($env:OS -ne 'Windows_NT') {
  throw 'この手順は Windows 専用です。'
}

$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [System.Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw '管理者 PowerShell で実行してください。'
}

# コマンドが無い場合も、古い $LASTEXITCODE を成功と誤認しないよう先に解決する。
$opensslCommand = if ($OpenSslPath) {
  Get-Command -Name $OpenSslPath -CommandType Application -ErrorAction Stop
} else {
  Get-Command -Name openssl.exe -CommandType Application -ErrorAction Stop
}
$openssl = $opensslCommand.Source
$version = @(& $openssl version)
if ($LASTEXITCODE -ne 0 -or $version.Count -ne 1) {
  throw 'OpenSSL の版を取得できませんでした。'
}
if (-not [System.IO.Path]::IsPathRooted($TlsDirectory)) {
  throw 'TlsDirectory は絶対パスで指定してください。'
}
$tlsDir = [System.IO.Path]::GetFullPath($TlsDirectory).TrimEnd('\')
$programData = [System.IO.Path]::GetFullPath(
  [System.Environment]::GetFolderPath([System.Environment+SpecialFolder]::CommonApplicationData)
).TrimEnd('\')
if (-not $env:ProgramData -or
    [System.IO.Path]::GetFullPath($env:ProgramData).TrimEnd('\') -ne $programData) {
  throw 'ProgramData の場所を確認できません。'
}
$defaultTlsDir = [System.IO.Path]::Combine($programData, 'BantoHub', 'tls')
$alternateTlsDir = [System.IO.Path]::Combine($programData, 'BantoHubTls')
if ($tlsDir -notin @($defaultTlsDir, $alternateTlsDir)) {
  throw 'TlsDirectory は ProgramData\BantoHub\tls または ProgramData\BantoHubTls を指定してください。'
}
$parentDir = [System.IO.Path]::GetDirectoryName($tlsDir)

$sidType = [System.Security.Principal.SecurityIdentifier]
$systemSid = 'S-1-5-18'
$adminsSid = 'S-1-5-32-544'
$trustedOwners = @($systemSid, $adminsSid)
$serviceSids = @($systemSid, $adminsSid)
$allowType = [System.Security.AccessControl.AccessControlType]::Allow
$inheritOnly = [System.Security.AccessControl.PropagationFlags]::InheritOnly
if ($identity.Owner.Value -notin $trustedOwners) {
  throw '昇格トークンの既定所有者が SYSTEM または Administrators ではありません。発行しません。'
}
# OPC UA の親ディレクトリ検査と同じ: DELETE / DELETE_CHILD / WRITE_DAC /
# WRITE_OWNER / GENERIC_ALL / GENERIC_WRITE。作成だけの権利は許す。
$unsafeParentMask = 0x500D0040

function Get-PlainDirectory {
  param([string]$Path)
  $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
  if (-not $item.PSIsContainer -or
      (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
    throw "通常のディレクトリではありません: $Path"
  }
  return $item
}

function Get-CheckedAcl {
  param([string]$Path)
  $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
  $raw = [System.Security.AccessControl.RawSecurityDescriptor]::new($acl.GetSecurityDescriptorBinaryForm(), 0)
  if ($null -eq $raw.DiscretionaryAcl -or $raw.DiscretionaryAcl.Count -eq 0) {
    throw "DACL を確認できません: $Path"
  }
  foreach ($ace in $raw.DiscretionaryAcl) {
    if ($ace -isnot [System.Security.AccessControl.KnownAce] -or
        $ace.AceType -notin @([System.Security.AccessControl.AceType]::AccessAllowed, [System.Security.AccessControl.AceType]::AccessDenied) -or
        $null -eq $ace.SecurityIdentifier) {
      throw "種類または SID の分からない ACE があります: $Path"
    }
  }
  $rules = @($acl.GetAccessRules($true, $true, $sidType))
  if ($rules.Count -eq 0) {
    throw "ACL を確認できません: $Path"
  }
  if ($acl.GetOwner($sidType).Value -notin $trustedOwners) {
    throw "所有者を信頼できません: $Path"
  }
  return $acl
}

function Assert-ServiceOnlyAcl {
  param([string]$Path, [bool]$MustBeProtected)
  $acl = Get-CheckedAcl $Path
  if ($MustBeProtected -and -not $acl.AreAccessRulesProtected) {
    throw "継承された ACL が残っています: $Path"
  }
  $rules = @($acl.GetAccessRules($true, $true, $sidType))
  foreach ($rule in $rules) {
    if ($rule.IdentityReference.Value -notin $serviceSids) {
      throw "SYSTEM と Administrators 以外の ACE があります: $Path"
    }
    if ($rule.AccessControlType -ne $allowType) {
      throw "サービス用の保管先に拒否 ACE があります: $Path"
    }
  }
  $inheritMask = [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
  foreach ($sid in $serviceSids) {
    $full = @($rules | Where-Object {
      $_.IdentityReference.Value -eq $sid -and
      $_.AccessControlType -eq $allowType -and
      (($_.PropagationFlags -band $inheritOnly) -eq 0) -and
      (-not $MustBeProtected -or (($_.InheritanceFlags -band $inheritMask) -eq $inheritMask)) -and
      (($_.FileSystemRights -band [System.Security.AccessControl.FileSystemRights]::FullControl) -eq [System.Security.AccessControl.FileSystemRights]::FullControl)
    })
    if ($full.Count -eq 0) {
      throw "SYSTEM または Administrators の FullControl がありません: $Path"
    }
  }
}

function Assert-SafeAncestors {
  # OS の ProgramData から保管先の親まで、入れ替え可能な祖先を残さない。
  # BantoHub は他のデータも持つため ACL を変更せず、危険な権限だけを拒否する。
  $ancestors = @($programData)
  if ($tlsDir -eq $defaultTlsDir) {
    $ancestors += $parentDir
  }
  foreach ($ancestor in $ancestors) {
    [void](Get-PlainDirectory $ancestor)
    $ancestorAcl = Get-CheckedAcl $ancestor
    foreach ($rule in $ancestorAcl.GetAccessRules($true, $true, $sidType)) {
      if ($rule.AccessControlType -ne $allowType -or
          $rule.IdentityReference.Value -in $trustedOwners -or
          (($rule.PropagationFlags -band $inheritOnly) -ne 0)) {
        continue
      }
      if (([int]$rule.FileSystemRights -band $unsafeParentMask) -ne 0) {
        throw "祖先に第三者の入れ替え権限があります: $ancestor"
      }
    }
  }
}

Assert-SafeAncestors

$existing = Get-Item -LiteralPath $tlsDir -Force -ErrorAction SilentlyContinue
if ($null -ne $existing) {
  # 既存の不安全な保管先は締め直して使い回さない。
  [void](Get-PlainDirectory $tlsDir)
  Assert-ServiceOnlyAcl $tlsDir $true
} else {
  $protectedAcl = [System.Security.AccessControl.DirectorySecurity]::new()
  $protectedAcl.SetOwner([System.Security.Principal.SecurityIdentifier]::new($adminsSid))
  $protectedAcl.SetAccessRuleProtection($true, $false)
  foreach ($sid in $serviceSids) {
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
      [System.Security.Principal.SecurityIdentifier]::new($sid),
      [System.Security.AccessControl.FileSystemRights]::FullControl,
      ([System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [System.Security.AccessControl.InheritanceFlags]::ObjectInherit),
      [System.Security.AccessControl.PropagationFlags]::None,
      $allowType
    )
    [void]$protectedAcl.AddAccessRule($rule)
  }
  $directory = [System.IO.DirectoryInfo]::new($tlsDir)
  if ($PSVersionTable.PSVersion.Major -lt 6) {
    [void]$directory.Create($protectedAcl)
  } else {
    [void][System.IO.FileSystemAclExtensions]::Create($directory, $protectedAcl)
  }
  [void](Get-PlainDirectory $tlsDir)
  Assert-ServiceOnlyAcl $tlsDir $true
}

# 作成名には日時と乱数を使い、既存の稼働中の鍵を上書きしない。
$stamp = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$configFile = Join-Path $tlsDir "hub-cert-$stamp.cnf"
$keyFile = Join-Path $tlsDir "hub-key-$stamp.pem"
$certFile = Join-Path $tlsDir "hub-cert-$stamp.pem"
foreach ($path in @($configFile, $keyFile, $certFile)) {
  if (Test-Path -LiteralPath $path) {
    throw "同名のファイルがあります。上書きしません: $path"
  }
}

$configCreated = $false
$opensslStarted = $false
try {
  @'
[req]
distinguished_name = dn
x509_extensions = v3
[dn]
[v3]
basicConstraints = critical,CA:FALSE
subjectAltName = IP:127.0.0.1
'@ | Out-File -LiteralPath $configFile -Encoding ascii -NoClobber -ErrorAction Stop
  $configCreated = $true
  Assert-ServiceOnlyAcl $configFile $false

  $opensslStarted = $true
  & $openssl req -x509 -config $configFile -newkey ec -pkeyopt ec_paramgen_curve:P-256 -days $Days -nodes -subj /CN=banto-hub -keyout $keyFile -out $certFile
  if ($LASTEXITCODE -ne 0 -or
      -not (Test-Path -LiteralPath $keyFile -PathType Leaf) -or
      -not (Test-Path -LiteralPath $certFile -PathType Leaf)) {
    throw '証明書を発行できませんでした。'
  }
  Assert-ServiceOnlyAcl $keyFile $false
  Assert-ServiceOnlyAcl $certFile $false

  $details = @(& $openssl x509 -in $certFile -noout -text)
  if ($LASTEXITCODE -ne 0) {
    throw '証明書の拡張を読めませんでした。'
  }
  $constraintLines = @(for ($i = 0; $i -lt $details.Count; $i++) {
    if ($details[$i] -match '^\s*X509v3 Basic Constraints:') { $i }
  })
  if ($constraintLines.Count -ne 1 -or
      $constraintLines[0] + 1 -ge $details.Count -or
      $details[$constraintLines[0] + 1] -notmatch '^\s*CA:FALSE\s*$') {
    throw 'Basic Constraints が 1 個の CA:FALSE ではありません。'
  }
  if (@($details | Where-Object { $_ -match 'IP Address:127\.0\.0\.1' }).Count -eq 0) {
    throw 'SAN に IP:127.0.0.1 がありません。'
  }

  $expiry = @(& $openssl x509 -in $certFile -noout -enddate)
  if ($LASTEXITCODE -ne 0 -or $expiry.Count -ne 1 -or $expiry[0] -notmatch '^notAfter=.+ GMT$') {
    throw '証明書の有効期限を読めませんでした。'
  }
  $fingerprint = @(& $openssl x509 -in $certFile -noout -fingerprint -sha256)
  if ($LASTEXITCODE -ne 0 -or $fingerprint.Count -ne 1 -or $fingerprint[0] -notmatch '^sha256 Fingerprint=([0-9A-F]{2}:){31}[0-9A-F]{2}$') {
    throw '証明書の SHA-256 指紋を読めませんでした。'
  }

  $result = [pscustomobject]@{
    CertificatePath = $certFile
    PrivateKeyPath = $keyFile
    ConfigPath = $configFile
    FingerprintSha256 = $fingerprint[0].Substring($fingerprint[0].IndexOf('=') + 1)
    NotAfterUtc = $expiry[0].Substring('notAfter='.Length)
    OpenSslPath = $openssl
    OpenSslVersion = $version[0]
  }
} catch {
  $originalError = $_
  $cleanupProblems = [System.Collections.Generic.List[string]]::new()
  $remaining = [System.Collections.Generic.List[string]]::new()
  $safeStore = $false
  try {
    Assert-SafeAncestors
    [void](Get-PlainDirectory $tlsDir)
    Assert-ServiceOnlyAcl $tlsDir $true
    $safeStore = $true
  } catch {
    $cleanupProblems.Add("保管先の再確認に失敗: $($_.Exception.Message)")
  }

  # 自分が作ったと確認できた通常ファイルだけを個別に削除する。
  foreach ($artifact in @(
    [pscustomobject]@{ Path = $configFile; Created = $configCreated },
    [pscustomobject]@{ Path = $keyFile; Created = $opensslStarted },
    [pscustomobject]@{ Path = $certFile; Created = $opensslStarted }
  )) {
    try {
      $item = Get-Item -LiteralPath $artifact.Path -Force -ErrorAction Stop
    } catch [System.Management.Automation.ItemNotFoundException] {
      continue
    } catch {
      $cleanupProblems.Add("対象の確認に失敗: $($artifact.Path): $($_.Exception.Message)")
      $remaining.Add($artifact.Path)
      continue
    }
    try {
      if (-not $safeStore -or -not $artifact.Created) {
        $remaining.Add($artifact.Path)
        continue
      }
      if ($item.PSIsContainer -or
          (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw '通常のファイルではありません。'
      }
      Assert-ServiceOnlyAcl $artifact.Path $false
      Remove-Item -LiteralPath $artifact.Path -Force -ErrorAction Stop
    } catch {
      $cleanupProblems.Add("後始末に失敗: $($artifact.Path): $($_.Exception.Message)")
      $remaining.Add($artifact.Path)
    }
  }
  if ($cleanupProblems.Count -gt 0 -or $remaining.Count -gt 0) {
    $cleanupDetail = if ($cleanupProblems.Count -gt 0) { $cleanupProblems -join ' / ' } else { '今回の作成物と確認できないファイルを残しました' }
    $message = "元のエラー: $($originalError.Exception.Message)。後始末: $cleanupDetail。残留の可能性があるパス: $($remaining -join ', ')。手動で所有者・ACL・リンクを確認し、安全を確かめてから削除してください。"
    throw [System.InvalidOperationException]::new($message, $originalError.Exception)
  }
  throw
}

$result
