#requires -Version 5.1
#requires -RunAsAdministrator
[CmdletBinding()]
param(
    [ValidateSet('Install', 'Status', 'Uninstall')][string]$Action = 'Install',
    [ValidateRange(1,65535)][int]$Port = 2222
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$serviceName = 'LanDeskServer'
$transportName = 'LanDeskTransport'
$firewallName = 'LanDesk-SSH-In-TCP'
$destination = Join-Path $env:ProgramFiles 'LanDeskServer'
$data = Join-Path $env:ProgramData 'LanDeskServer'
$transportData = Join-Path $data 'transport'
$configPath = Join-Path $transportData 'config.json'
$ownerText = 'LanDeskServer console service v1'
$policyPath = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
$policyName = 'SoftwareSASGeneration'
$shortcut = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'LanDeskServer.lnk'
$runPath = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run'
$uiPath = 'HKLM:\SOFTWARE\LanDesk\Server'
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
function Assert-Owned([string]$Path, [switch]$TransportRead, [switch]$UsersRead) {
    Assert-PlainDirectory $Path
    $acl = Get-Acl -LiteralPath $Path
    $trusted = @('S-1-5-18', 'S-1-5-32-544')
    if (!$acl.AreAccessRulesProtected -or $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -notin $trusted) {
        throw "Installation directory has an untrusted owner or inherited permissions: $Path"
    }
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $trusted) {
            if (!(($TransportRead -and $rule.IdentityReference.Value -eq 'S-1-5-19') -or ($UsersRead -and $rule.IdentityReference.Value -eq 'S-1-5-32-545')) -or ($rule.FileSystemRights -band (-bnot ([Security.AccessControl.FileSystemRights]::ReadAndExecute -bor [Security.AccessControl.FileSystemRights]::Synchronize))) -ne 0) {
                throw "Installation directory grants access to an untrusted principal: $Path"
            }
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
function New-ProtectedDirectory([string]$Path, [switch]$TransportRead, [switch]$UsersRead) {
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $sddl = 'O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)'
    if ($TransportRead) { $sddl += '(A;OICI;0x1200a9;;;LS)' }
    if ($UsersRead) { $sddl += '(A;OICI;0x1200a9;;;BU)' }
    $acl.SetSecurityDescriptorSddlForm($sddl)
    ([IO.DirectoryInfo]::new($Path)).Create($acl)
    [IO.File]::WriteAllText((Join-Path $Path 'landesk-owner.txt'), $ownerText, $utf8)
    Assert-Owned $Path -TransportRead:$TransportRead -UsersRead:$UsersRead
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
    Assert-Owned $destination -TransportRead -UsersRead
    foreach ($entry in @(@($serviceName, '--service'), @($transportName, '--transport-service'))) {
        $existing = Get-CimInstance Win32_Service -Filter ("Name='" + $entry[0] + "'")
        if ($null -ne $existing -and $existing.PathName -ne ('"' + (Join-Path $destination 'LanDeskServer.exe') + '" ' + $entry[1])) {
            throw "An unrelated service uses the $($entry[0]) name. No service was changed."
        }
    }
}
function Stop-OwnedService([string]$Name) {
    $service = Get-Service -Name $Name -ErrorAction SilentlyContinue
    if ($null -ne $service -and $service.Status -ne 'Stopped') {
        Stop-Service $Name
        $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(20))
    }
}
function Get-TransportInfo {
    & (Join-Path $PSScriptRoot 'LanDeskServer.exe') --transport-info
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read transport configuration.' }
}
function Show-Status {
    Get-Service -Name $serviceName,$transportName -ErrorAction SilentlyContinue | Select-Object Name, Status, StartType
    & (Join-Path $PSScriptRoot 'LanDeskDisplay.exe') --status
    if ($LASTEXITCODE -ne 0) { throw 'Display status failed.' }
    if (Test-Path -LiteralPath $configPath) { Get-TransportInfo }
    Write-Host "Logs: $(Join-Path $data 'server.log'), $(Join-Path $data 'transport.log')"
}
function New-TransportConfig {
    $secure = Read-Host 'Set LanDesk access password (leave empty to generate)' -AsSecureString
    $pointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try { $password = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($pointer) }
    finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($pointer); $secure.Dispose() }
    $generated = [string]::IsNullOrEmpty($password)
    if ($generated) {
        $random = New-Object byte[] 18
        $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
        try { $rng.GetBytes($random) } finally { $rng.Dispose() }
        $password = [Convert]::ToBase64String($random)
    }
    # The password travels over a private pipe, never process arguments or a file.
    $process = [Diagnostics.Process]::new()
    $process.StartInfo.FileName = Join-Path $PSScriptRoot 'LanDeskServer.exe'
    $process.StartInfo.Arguments = '--configure-transport'
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardInput = $true
    try {
        if (!$process.Start()) { throw 'Cannot start transport configuration.' }
        $payload = $utf8.GetBytes((@{password=$password;port=$Port} | ConvertTo-Json -Compress))
        try {
            $process.StandardInput.BaseStream.Write($payload, 0, $payload.Length)
            $process.StandardInput.BaseStream.Close()
        } finally { [Array]::Clear($payload, 0, $payload.Length) }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw 'Transport configuration failed.' }
        if ($generated) { Write-Host "Generated LanDesk password (save it now): $password" }
    } finally { $password = $null; $process.Dispose() }
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
        Assert-Owned $data
        $rule = Get-NetFirewallRule -Name $firewallName -ErrorAction SilentlyContinue
        if ($null -ne $rule -and $rule.Group -ne 'LanDesk managed transport') { throw 'Firewall rule ownership mismatch. Nothing was removed.' }
        $run = Get-ItemProperty -LiteralPath $runPath -Name $serviceName -ErrorAction SilentlyContinue
        if ($null -ne $run -and $run.$serviceName -eq ('"' + (Join-Path $destination 'LanDeskServer.exe') + '" --startup-ui')) { Remove-ItemProperty -LiteralPath $runPath -Name $serviceName }
        if (Test-Path -LiteralPath $shortcut) {
            $link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
            if ($link.TargetPath -eq (Join-Path $destination 'LanDeskServer.exe')) { Remove-Item -LiteralPath $shortcut }
        }
        # Close only installed settings/tray processes, not unrelated package executables.
        foreach ($process in Get-CimInstance Win32_Process -Filter "Name='LanDeskServer.exe'") {
            if ($process.ExecutablePath -eq (Join-Path $destination 'LanDeskServer.exe') -and $process.CommandLine -notmatch '--(service|transport-service|worker|set-startup)') {
                Stop-Process -Id $process.ProcessId
            }
        }
        foreach ($name in @($serviceName, $transportName)) {
            if (Get-Service -Name $name -ErrorAction SilentlyContinue) {
                Stop-OwnedService $name
                Invoke-Sc -Arguments @('delete', $name)
            }
        }
        if ($null -ne $rule) { Remove-NetFirewallRule -Name $firewallName }
        Restore-Policy
        # Delete only files shipped by this installer; retain unexpected files.
        Remove-Item -LiteralPath (Join-Path $destination 'LanDeskServer.exe')
        Remove-Item -LiteralPath (Join-Path $destination 'landesk-owner.txt')
        if (@(Get-ChildItem -LiteralPath $destination -Force).Count -eq 0) { [IO.Directory]::Delete($destination) }
        Write-Host 'Server removed. LanDesk credentials/host identity, logs and virtual display were retained. System OpenSSH was not changed.'
        Write-Host 'To remove the display: .\Install-VirtualDisplay.ps1 -Action Uninstall'
        return
    }
    if (Get-Service -Name $serviceName,$transportName -ErrorAction SilentlyContinue) { throw 'LanDesk is already installed. Uninstall it before installing this build; credentials and host identity are retained.' }
    if (Get-NetFirewallRule -Name $firewallName -ErrorAction SilentlyContinue) { throw 'LanDesk firewall rule already exists. Inspect it before retrying.' }
    if (Test-Path -LiteralPath $shortcut) { throw 'LanDeskServer start menu shortcut already exists. Inspect it before retrying.' }
    if (Get-ItemProperty -LiteralPath $runPath -Name $serviceName -ErrorAction SilentlyContinue) { throw 'LanDeskServer startup entry already exists. Inspect it before retrying.' }
    if (Test-Path -LiteralPath $destination) { throw "Installation directory already exists: $destination. Inspect it before retrying." }
    foreach ($file in @('LanDeskServer.exe', 'LanDeskDisplay.exe', 'Install-VirtualDisplay.ps1')) {
        if (!(Test-Path -LiteralPath (Join-Path $PSScriptRoot $file) -PathType Leaf)) { throw "Missing package file: $file" }
    }
    # Existing organisation policy wins. Never weaken a prohibition on software SAS.
    $policy = Get-Policy
    if ($null -ne $policy -and $policy -notin @(1, 3)) { throw 'Existing policy prohibits service-generated Ctrl+Alt+Delete. Ask your administrator to allow services before installation.' }
    if (Test-Path -LiteralPath $data) { Assert-Owned $data } else { New-ProtectedDirectory $data }
    if (Test-Path -LiteralPath $transportData) { Assert-Owned $transportData -TransportRead } else { New-ProtectedDirectory $transportData -TransportRead }
    if (!(Test-Path -LiteralPath $configPath)) { New-TransportConfig }
    $info = Get-TransportInfo | ConvertFrom-Json
    if ($PSBoundParameters.ContainsKey('Port') -and $Port -ne $info.port) { throw 'Stored SSH port differs. Use the existing port when reinstalling.' }
    $Port = [int]$info.port
    $transportLog = Join-Path $data 'transport.log'
    if (!(Test-Path -LiteralPath $transportLog)) { [IO.File]::WriteAllText($transportLog, '', $utf8) }
    $logAcl = [Security.AccessControl.FileSecurity]::new()
    $logAcl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGW;;;LS)')
    Set-Acl -LiteralPath $transportLog -AclObject $logAcl
    $before = & (Join-Path $PSScriptRoot 'LanDeskDisplay.exe') --status | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Display status failed.' }
    $createdService = $false
    $createdTransport = $false
    $createdFirewall = $false
    $createdDestination = $false
    $createdStartup = $false
    $createdShortcut = $false
    try {
        & (Join-Path $PSScriptRoot 'Install-VirtualDisplay.ps1')
        New-ProtectedDirectory $destination -TransportRead -UsersRead
        $createdDestination = $true
        [IO.File]::Copy((Join-Path $PSScriptRoot 'LanDeskServer.exe'), (Join-Path $destination 'LanDeskServer.exe'), $false)
        if ($null -eq $policy) {
            [IO.File]::WriteAllText((Join-Path $data 'sas-policy-created.txt'), '1', $utf8)
            New-ItemProperty -LiteralPath $policyPath -Name $policyName -PropertyType DWord -Value 1 | Out-Null
        }
        Invoke-Sc -Arguments @('create', $transportName, 'binPath=', ('"' + (Join-Path $destination 'LanDeskServer.exe') + '" --transport-service'), 'start=', 'demand', 'obj=', 'NT AUTHORITY\LocalService', 'DisplayName=', 'LanDesk Encrypted Transport')
        $createdTransport = $true
        Invoke-Sc -Arguments @('failure', $transportName, 'reset=', '86400', 'actions=', 'restart/5000/restart/15000/none/0')
        Invoke-Sc -Arguments @('failureflag', $transportName, '1')
        New-NetFirewallRule -Name $firewallName -DisplayName 'LanDesk encrypted desktop' -Group 'LanDesk managed transport' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort $Port -Program (Join-Path $destination 'LanDeskServer.exe') -Profile Any | Out-Null
        $createdFirewall = $true
        New-Service -Name $serviceName -DisplayName 'LanDesk Remote Desktop' -BinaryPathName ('"' + (Join-Path $destination 'LanDeskServer.exe') + '" --service') -StartupType Automatic -DependsOn $transportName -Description 'LanDesk console desktop capture and input with embedded encrypted transport.' | Out-Null
        $createdService = $true
        Invoke-Sc -Arguments @('failure', $serviceName, 'reset=', '86400', 'actions=', 'restart/5000/restart/15000/none/0')
        Invoke-Sc -Arguments @('failureflag', $serviceName, '1')
        Start-Service $serviceName
        $service = Get-Service $serviceName
        $service.WaitForStatus('Running', [TimeSpan]::FromSeconds(20))
        # SCM running does not prove that the session worker accepts authenticated HTTP over its pipe.
        $ready = $false
        for ($i = 0; $i -lt 20; $i++) {
            try {
                & (Join-Path $destination 'LanDeskServer.exe') --desktop-check
                if ($LASTEXITCODE -eq 0) { $ready = $true; break }
                Start-Sleep -Milliseconds 500
            } catch { Start-Sleep -Milliseconds 500 }
        }
        if (!$ready) { throw "Service worker did not become ready. Read $(Join-Path $data 'server.log')" }
        $minimum = 1
        $saved = Get-ItemProperty -LiteralPath $uiPath -Name MinimizeOnStart -ErrorAction SilentlyContinue
        if ($null -ne $saved) { $minimum = [int][bool]$saved.MinimizeOnStart }
        & (Join-Path $destination 'LanDeskServer.exe') --set-startup 1 $minimum
        if ($LASTEXITCODE -ne 0) { throw 'Startup settings failed.' }
        $createdStartup = $true
        $link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
        $link.TargetPath = Join-Path $destination 'LanDeskServer.exe'
        $link.Arguments = '--settings'
        $link.Save()
        $createdShortcut = $true
    } catch {
        $failure = $_
        try {
            if ($createdShortcut) { Remove-Item -LiteralPath $shortcut }
            if ($createdStartup) {
                Remove-ItemProperty -LiteralPath $runPath -Name $serviceName
                if ($null -eq $saved) { Remove-ItemProperty -LiteralPath $uiPath -Name MinimizeOnStart }
            }
            if ($createdService) {
                $service = Get-Service $serviceName
                if ($service.Status -ne 'Stopped') { Stop-Service $serviceName; $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(20)) }
                Invoke-Sc -Arguments @('delete', $serviceName)
            }
            if ($createdTransport) { Stop-OwnedService $transportName; Invoke-Sc -Arguments @('delete', $transportName) }
            if ($createdFirewall) { Remove-NetFirewallRule -Name $firewallName }
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
    Write-Host "LanDeskServer installed. Client: Desktop / IP / port $Port / user landesk / LanDesk password."
    Write-Host 'No Windows OpenSSH or second Web password is required. Windows login stays inside the remote desktop.'
    Write-Host 'For reboot-required display installation, restart Windows before testing video.'
    Show-Status
    Start-Process -FilePath (Join-Path $destination 'LanDeskServer.exe') -ArgumentList '--settings'
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
