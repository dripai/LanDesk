#requires -Version 5.1
#requires -RunAsAdministrator
[CmdletBinding()]
param([ValidateSet('Install', 'Status', 'Uninstall')][string]$Action = 'Install')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$exe = Join-Path $PSScriptRoot 'LanDeskDisplay.exe'
$config = 'C:\VirtualDisplayDriver'
$settings = Join-Path $config 'vdd_settings.xml'
$owner = Join-Path $config 'landesk-owner.txt'
$ownerText = 'LanDesk ROOT\LANDESKDISPLAY\0000 v1'
$template = Join-Path $PSScriptRoot 'driver\vdd_settings.xml'
$registry = 'HKLM:\SOFTWARE\MikeTheTech\VirtualDisplayDriver'

function Invoke-Display([string]$Command) {
    $json = & $exe $Command
    if ($LASTEXITCODE -ne 0) { throw "LanDeskDisplay $Command failed (exit $LASTEXITCODE)." }
    return ($json | ConvertFrom-Json)
}
function Assert-OwnedConfig {
    if (!(Test-Path -LiteralPath $owner -PathType Leaf) -or
        [IO.File]::ReadAllText($owner) -ne $ownerText) {
        throw "Existing $config is not owned by LanDesk. No files were changed."
    }
    if ((Get-FileHash -LiteralPath $settings -Algorithm SHA256).Hash -ne
        (Get-FileHash -LiteralPath $template -Algorithm SHA256).Hash) {
        throw 'Virtual display settings were changed. Restore the bundled settings before reinstalling.'
    }
}

if (![Environment]::Is64BitProcess -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'This package requires Windows x64 and 64-bit PowerShell.'
}
if (!(Test-Path -LiteralPath $exe -PathType Leaf)) { throw "Missing $exe. Extract the complete package first." }
$mutex = [Threading.Mutex]::new($false, 'Global\LanDeskVirtualDisplaySetup')
$locked = $false
try {
    try { $locked = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (!$locked) { throw 'Another LanDesk display setup is running.' }
    if ($Action -eq 'Status') { Invoke-Display '--status'; return }
    if ($Action -eq 'Uninstall') {
        $result = Invoke-Display '--uninstall'
        $result
        Write-Host 'LanDesk device removed. Configuration and shared Driver Store package were retained.'
        if ($result.reboot_required) { Write-Host 'Windows requires a restart to complete removal.' }
        return
    }

    # These are the original, byte-identical files from the 25.7.23 upstream release.
    $hashes = @{
        'MttVDD.dll' = 'C9CA837F57A98FBD43BC416A7F535A95843626E7759EAF85CF0CD7CE334DBB05'
        'MttVDD.inf' = '550D211FE481E74DFE3F9D724ED78BE48B3A9113405965D683D9373E8D672F5D'
        'mttvdd.cat' = '08A0093FC9B2E32B287A6F8A77CA4DE0A31830D29FC33D2B13A918DC859468F6'
    }
    foreach ($name in $hashes.Keys) {
        $file = Join-Path $PSScriptRoot "driver\$name"
        if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $hashes[$name]) {
            throw "Bundled driver hash mismatch: $name"
        }
    }
    $signature = Get-AuthenticodeSignature -LiteralPath (Join-Path $PSScriptRoot 'driver\mttvdd.cat')
    if ($signature.Status -ne 'Valid') { throw "Driver catalog signature is not trusted: $($signature.Status)" }
    if (Test-Path -LiteralPath $registry) {
        throw 'Existing MttVDD global registry configuration detected. Remove it through its original installer first.'
    }
    $state = Invoke-Display '--status'
    $null = Invoke-Display '--check-install'
    $createdConfig = $false
    if (Test-Path -LiteralPath $config) {
        Assert-OwnedConfig
    } else {
        if ($state.installed) { throw 'LanDesk display is installed but its configuration is missing. Uninstall first.' }
        # Creating with the ACL avoids a writable configuration directory between steps.
        $acl = [Security.AccessControl.DirectorySecurity]::new()
        $acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;AU)')
        $directory = [IO.DirectoryInfo]::new($config)
        $directory.Create($acl)
        $createdConfig = $true
    }
    try {
        if ($createdConfig) {
            [IO.File]::WriteAllText($owner, $ownerText, [Text.UTF8Encoding]::new($false))
            [IO.File]::Copy($template, $settings, $false)
        }
        $result = Invoke-Display '--install'
        if ($result.PSObject.Properties.Name -contains 'reboot_required' -and $result.reboot_required) {
            Write-Host 'Driver installed. Windows requires a restart; run -Action Status afterward.'
        } elseif (!$result.started) {
            throw "Driver is installed but not running (problem $($result.problem))."
        } else {
            Write-Host 'LanDesk virtual display driver started. Verify the screen in Windows display settings.'
        }
        $result
    } catch {
        $failure = $_
        # Never remove settings while a device remains after a failed rollback.
        if ($createdConfig) {
            $after = Invoke-Display '--status'
            if (!$after.installed) {
                if (Test-Path -LiteralPath $settings) { Remove-Item -LiteralPath $settings }
                if (Test-Path -LiteralPath $owner) { Remove-Item -LiteralPath $owner }
                # Delete only an empty directory; driver-created logs are preserved.
                if (@(Get-ChildItem -LiteralPath $config -Force).Count -eq 0) { [IO.Directory]::Delete($config) }
            } else {
                Write-Warning 'Device still exists; configuration retained. Run -Action Uninstall before retrying.'
            }
        }
        throw $failure
    }
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
