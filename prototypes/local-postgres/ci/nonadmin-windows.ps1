# Create a standard local user (member of Users only, not Administrators) and
# run two Postgres sessions as that user, with its own profile and LOCALAPPDATA.
# usage: pwsh ci/nonadmin-windows.ps1 <lp.exe> <pg-version> <zonky|theseus>
param([string]$Bin, [string]$Ver, [string]$Dist)
$ErrorActionPreference = 'Continue'

$pw = 'Lp-' + [guid]::NewGuid().ToString('N') + 'a1!'   # throwaway
$sec = ConvertTo-SecureString $pw -AsPlainText -Force
New-LocalUser -Name lpuser -Password $sec -AccountNeverExpires -PasswordNeverExpires | Out-Null
Add-LocalGroupMember -Group Users -Member lpuser
"lpuser groups: " + ((Get-LocalGroup | Where-Object { (Get-LocalGroupMember $_ -ErrorAction SilentlyContinue).Name -match 'lpuser' }).Name -join ', ')

$dir = 'C:\Users\Public\lp-na'
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $Bin "$dir\lp.exe"
icacls $dir /grant 'Users:(OI)(CI)M' | Out-Null
$cred = New-Object System.Management.Automation.PSCredential("$env:COMPUTERNAME\lpuser", $sec)

function As-User([string]$exe, [string[]]$argv, [string]$tag) {
  try {
    $p = Start-Process -FilePath $exe -ArgumentList $argv -Credential $cred -LoadUserProfile `
         -WorkingDirectory $dir -RedirectStandardOutput "$dir\$tag.out" -RedirectStandardError "$dir\$tag.err" `
         -Wait -PassThru -ErrorAction Stop
    Get-Content "$dir\$tag.out" -Encoding utf8 | Out-Host
    Get-Content "$dir\$tag.err" -Encoding utf8 | Out-Host
    Write-Host "exit code ($tag): $($p.ExitCode)"
    return $p.ExitCode
  } catch {
    Write-Host "Start-Process as lpuser failed ($tag): $_"
    return 99
  }
}

$null = As-User 'C:\Windows\System32\whoami.exe' @('/groups') 'whoami'
$null = As-User 'C:\Windows\System32\whoami.exe' @('/priv') 'priv'
$status = 0
foreach ($i in 1, 2) {
  "--- non-admin session $i"
  $rc = As-User "$dir\lp.exe" @('pg', '"~/lp spike Zoë 数据"', $Ver, $Dist) "run$i"
  if ($rc -ne 0) { $status = 1 }
}
$ud = 'C:\Users\lpuser\AppData\Local\lp spike Zoë 数据'
"data dir exists under lpuser's LOCALAPPDATA: $(Test-Path -LiteralPath $ud)"
if ($status -eq 0) { 'NONADMIN: PASS' } else { 'NONADMIN: FAIL' }
exit $status
