#requires -Version 5.1
#requires -RunAsAdministrator
[CmdletBinding()]
param(
    [ValidateSet('Install', 'Status', 'Uninstall')][string]$Action = 'Install',
    [switch]$InstallOpenSSH,
    [switch]$ShowAccessKey
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$serviceName = 'LanDeskServer'
$destination = Join-Path $env:ProgramFiles 'LanDeskServer'
$data = Join-Path $env:ProgramData 'LanDeskServer'
$ownerText = 'LanDeskServer console service v1'
$policyPath = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
$policyName = 'SoftwareSASGeneration'
$utf8 = [Text.UTF8Encoding]::new($false)

function Invoke-Sc([string[]]$Arguments) {
    & "$env:SystemRoot\System32\sc.exe" @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Service command failed: $($Arguments[0]) ($LASTEXITCODE)" }
}
function Assert-PlainDirectory([string]$Path) {
    if ((Get-Item -LiteralPath $Path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Refusing reparse point: $Path"
    }
}
function Assert-Owned([string]$Path) {
    Assert-PlainDirectory $Path
    $acl = Get-Acl -LiteralPath $Path
    $trusted = @('S-1-5-18', 'S-1-5-32-544')
    if (!$acl.AreAccessRulesProtected -or $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -notin $trusted) {
        throw "Installation directory has an untrusted owner or inherited permissions: $Path"
    }
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $trusted) {
            throw "Installation directory grants access to an untrusted principal: $Path"
        }
    }
    $marker = Join-Path $Path 'landesk-owner.txt'
    if (!(Test-Path -LiteralPath $marker -PathType Leaf) -or [IO.File]::ReadAllText($marker) -ne $ownerText) {
        throw "Existing directory is not owned by this installer: $Path"
    }
    foreach ($file in Get-ChildItem -LiteralPath $Path -Force) {
        if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing reparse point: $($file.FullName)" }
    }
}
function New-ProtectedDirectory([string]$Path) {
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')
    ([IO.DirectoryInfo]::new($Path)).Create($acl)
    [IO.File]::WriteAllText((Join-Path $Path 'landesk-owner.txt'), $ownerText, $utf8)
    Assert-Owned $Path
}
function Get-Policy {
    $key = Get-Item -LiteralPath $policyPath
    if ($key.GetValueNames() -contains $policyName) { return [int]$key.GetValue($policyName) }
    return $null
}
function Restore-Policy {
    $marker = Join-Path $data 'sas-policy-created.txt'
    if (Test-Path -LiteralPath $marker) {
        if ((Get-Policy) -eq 1) { Remove-ItemProperty -LiteralPath $policyPath -Name $policyName }
        else { Write-Warning 'SAS policy was subsequently changed; leaving it unchanged.' }
        Remove-Item -LiteralPath $marker
    }
}
function Assert-ServiceOwned {
    Assert-Owned $destination
    $existing = Get-CimInstance Win32_Service -Filter "Name='LanDeskServer'"
    if ($null -ne $existing -and $existing.PathName -ne ('"' + (Join-Path $destination 'LanDeskServer.exe') + '" --service')) {
        throw 'An unrelated service uses the LanDeskServer name. No service was changed.'
    }
}
function Show-Status {
    Get-Service -Name $serviceName -ErrorAction SilentlyContinue | Select-Object Name, Status, StartType
    & (Join-Path $PSScriptRoot 'LanDeskDisplay.exe') --status
    if ($LASTEXITCODE -ne 0) { throw 'Display status failed.' }
    Get-Service -Name sshd -ErrorAction SilentlyContinue | Select-Object Name, Status, StartType
    Write-Host "Log: $(Join-Path $data 'server.log')"
    if ($ShowAccessKey) {
        Assert-Owned $data
        Write-Host ('Web access key: ' + [IO.File]::ReadAllText((Join-Path $data 'access-key.txt')))
    }
}
if (![Environment]::Is64BitProcess -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'Use 64-bit Windows PowerShell on Windows x64.' }
$mutex = [Threading.Mutex]::new($false, 'Global\LanDeskServerSetup')
$locked = $false
try {
    try { $locked = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (!$locked) { throw 'Another LanDeskServer setup is running.' }
    if ($Action -eq 'Status') { Show-Status; return }
    if ($Action -eq 'Uninstall') {
        Assert-ServiceOwned
        $service = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
        if ($null -ne $service) {
            if ($service.Status -ne 'Stopped') { Stop-Service $serviceName; $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(20)) }
            Invoke-Sc -Arguments @('delete', $serviceName)
        }
        Assert-Owned $data
        Restore-Policy
        # Delete only files shipped by this installer; retain unexpected files.
        Remove-Item -LiteralPath (Join-Path $destination 'LanDeskServer.exe')
        Remove-Item -LiteralPath (Join-Path $destination 'landesk-owner.txt')
        if (@(Get-ChildItem -LiteralPath $destination -Force).Count -eq 0) { [IO.Directory]::Delete($destination) }
        Write-Host 'Server removed. Access key/logs, virtual display, OpenSSH and its firewall rules were retained.'
        Write-Host 'To remove the display: .\Install-VirtualDisplay.ps1 -Action Uninstall'
        return
    }
    if (Get-Service -Name $serviceName -ErrorAction SilentlyContinue) { throw 'LanDeskServer is already installed. Uninstall it before installing this build; the access key is retained.' }
    if (Test-Path -LiteralPath $destination) { throw "Installation directory already exists: $destination. Inspect it before retrying." }
    foreach ($file in @('LanDeskServer.exe', 'LanDeskDisplay.exe', 'Install-VirtualDisplay.ps1')) {
        if (!(Test-Path -LiteralPath (Join-Path $PSScriptRoot $file) -PathType Leaf)) { throw "Missing package file: $file" }
    }
    # Existing organisation policy wins. Never weaken a prohibition on software SAS.
    $policy = Get-Policy
    if ($null -ne $policy -and $policy -notin @(1, 3)) { throw 'Existing policy prohibits service-generated Ctrl+Alt+Delete. Ask your administrator to allow services before installation.' }
    if (Test-Path -LiteralPath $data) { Assert-Owned $data } else { New-ProtectedDirectory $data }
    $keyPath = Join-Path $data 'access-key.txt'
    if (!(Test-Path -LiteralPath $keyPath)) {
        $random = New-Object byte[] 32
        $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
        try { $rng.GetBytes($random) } finally { $rng.Dispose() }
        [IO.File]::WriteAllText($keyPath, ([BitConverter]::ToString($random).Replace('-', '').ToLowerInvariant()), $utf8)
    }
    if ([IO.File]::ReadAllText($keyPath) -notmatch '\A[0-9a-f]{64}\z') { throw 'Stored access key is invalid.' }
    $before = & (Join-Path $PSScriptRoot 'LanDeskDisplay.exe') --status | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Display status failed.' }
    $createdService = $false
    $createdDestination = $false
    try {
        & (Join-Path $PSScriptRoot 'Install-VirtualDisplay.ps1')
        New-ProtectedDirectory $destination
        $createdDestination = $true
        [IO.File]::Copy((Join-Path $PSScriptRoot 'LanDeskServer.exe'), (Join-Path $destination 'LanDeskServer.exe'), $false)
        if ($null -eq $policy) {
            [IO.File]::WriteAllText((Join-Path $data 'sas-policy-created.txt'), '1', $utf8)
            New-ItemProperty -LiteralPath $policyPath -Name $policyName -PropertyType DWord -Value 1 | Out-Null
        }
        New-Service -Name $serviceName -DisplayName 'LanDesk Remote Desktop' -BinaryPathName ('"' + (Join-Path $destination 'LanDeskServer.exe') + '" --service') -StartupType Automatic -Description 'LanDesk console desktop capture and input; SSH tunnel and access key required.' | Out-Null
        $createdService = $true
        Invoke-Sc -Arguments @('failure', $serviceName, 'reset=', '86400', 'actions=', 'restart/5000/restart/15000/none/0')
        Start-Service $serviceName
        $service = Get-Service $serviceName
        $service.WaitForStatus('Running', [TimeSpan]::FromSeconds(20))
        # SCM running does not prove that the session worker bound its HTTP port.
        $ready = $false
        for ($i = 0; $i -lt 20; $i++) {
            try {
                $page = Invoke-WebRequest 'http://127.0.0.1:17890/' -UseBasicParsing -TimeoutSec 2
                if ($page.Content.Contains('id="access-key-field"><label')) { $ready = $true; break }
            } catch { Start-Sleep -Milliseconds 500 }
        }
        if (!$ready) { throw "Service worker did not become ready. Read $(Join-Path $data 'server.log')" }
    } catch {
        $failure = $_
        try {
            if ($createdService) {
                $service = Get-Service $serviceName
                if ($service.Status -ne 'Stopped') { Stop-Service $serviceName; $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(20)) }
                Invoke-Sc -Arguments @('delete', $serviceName)
            }
            Restore-Policy
            if ($createdDestination) {
                foreach ($file in @('LanDeskServer.exe', 'landesk-owner.txt')) {
                    $path = Join-Path $destination $file
                    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
                }
                if (@(Get-ChildItem -LiteralPath $destination -Force).Count -eq 0) { [IO.Directory]::Delete($destination) }
            }
            if (!$before.installed) { & (Join-Path $PSScriptRoot 'Install-VirtualDisplay.ps1') -Action Uninstall }
        } catch { Write-Warning "Rollback incomplete: $_. Inspect service/device status before retrying." }
        throw $failure
    }
    # Explicit opt-in: install SSH transport without changing existing sshd_config,
    # account passwords or authentication rules. Failure is reported separately.
    if ($InstallOpenSSH) {
        try {
            $capability = Get-WindowsCapability -Online -Name 'OpenSSH.Server~~~~0.0.1.0'
            if ($capability.State -ne 'Installed') { Add-WindowsCapability -Online -Name $capability.Name | Out-Null }
            Set-Service sshd -StartupType Automatic
            Start-Service sshd
            if (!(Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue)) {
                New-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -DisplayName 'OpenSSH Server (sshd)' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 -Profile Domain,Private -RemoteAddress LocalSubnet | Out-Null
            }
        } catch { throw "LanDesk installed, but requested OpenSSH setup FAILED: $_. SSH component/policy changes may remain; inspect sshd and firewall status." }
    }
    Write-Host 'LanDeskServer installed. Select Desktop mode in LanDeskClient and connect over SSH.'
    Write-Host ('Web access key (keep private): ' + [IO.File]::ReadAllText($keyPath))
    if (!(Get-Service sshd -ErrorAction SilentlyContinue)) { Write-Warning 'OpenSSH Server is missing. Install/enable it before connecting with LanDeskClient.' }
    Write-Host 'For reboot-required display installation, restart Windows before testing video.'
    Show-Status
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
