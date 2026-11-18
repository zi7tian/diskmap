# GUI 子系统程序必须等待，并分别重定向标准输入、输出、错误。
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
$out = Join-Path $root 'target/packaging/out'
$work = Join-Path $root ('target/packaging/smoke-windows-' + [guid]::NewGuid())
New-Item -ItemType Directory -Force $work | Out-Null
$env:DISKMAP_CACHE_DIR = Join-Path $work 'cache'
$env:DISKMAP_CONFIG_DIR = Join-Path $work 'config'
$env:DISKMAP_NO_ATTACH = '1'
$env:DISKMAP_LANG = 'en'
New-Item -ItemType Directory -Force $env:DISKMAP_CACHE_DIR, $env:DISKMAP_CONFIG_DIR | Out-Null
# 卸载后应保留这两个隔离目录中的内容。
$cacheMarker = Join-Path $env:DISKMAP_CACHE_DIR 'keep.txt'
$configMarker = Join-Path $env:DISKMAP_CONFIG_DIR 'keep.txt'
Set-Content $cacheMarker 'cache-marker' -Encoding utf8
Set-Content $configMarker 'config-marker' -Encoding utf8

function Assert-PreservedData {
    if ((Get-Content $cacheMarker -Raw).Trim() -cne 'cache-marker' -or
        (Get-Content $configMarker -Raw).Trim() -cne 'config-marker') {
        throw '卸载删除了数据 / Uninstallation removed application data'
    }
}

function Invoke-CheckedProcess([string]$File, [string]$Arguments) {
    $process = Start-Process -FilePath $File -ArgumentList $Arguments -Wait -PassThru
    if ($process.ExitCode -notin 0, 3010) {
        throw "安装或卸载失败 / Installer failed ($($process.ExitCode)) $File"
    }
}

function Wait-NsisUninstall([string]$Directory) {
    $files = @('uninstall.exe', 'DiskMap.exe', 'diskmap-cli.exe') |
        ForEach-Object { Join-Path $Directory $_ }
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        $remaining = @($files | Where-Object { Test-Path -LiteralPath $_ })
        if ($remaining.Count -eq 0) { return }
        if ($timer.Elapsed.TotalSeconds -ge 120) {
            throw ("NSIS 卸载超时（120 秒），残留文件 / NSIS uninstall timed out (120s), remaining files`n{0}" -f ($remaining -join "`n"))
        }
        Start-Sleep -Milliseconds 500
    }
}

function Test-Program([string]$Program, [string]$Label) {
    if (-not (Test-Path $Program -PathType Leaf)) {
        throw "程序缺失 / Missing executable $Program"
    }
    $prefix = Join-Path $work $Label
    $report = "$prefix.report.json"
    $process = Start-Process -FilePath $Program -Wait -PassThru -NoNewWindow `
        -ArgumentList ('--selftest --report "{0}"' -f $report) `
        -RedirectStandardOutput "$prefix.selftest.out" -RedirectStandardError "$prefix.selftest.err"
    if ($process.ExitCode -ne 0) {
        Get-Content "$prefix.selftest.err"
        if (Test-Path $report) { Get-Content $report }
        throw "自检进程失败 / Self-test process failed $Program"
    }
    $data = Get-Content $report -Raw -Encoding utf8 | ConvertFrom-Json
    if ($data.ok -ne $true -or $data.frozen -ne $true) {
        throw "自检报告失败 / Self-test report failed $report"
    }
    $process = Start-Process -FilePath $Program -ArgumentList '--version' -Wait -PassThru -NoNewWindow `
        -RedirectStandardOutput "$prefix.version.out" -RedirectStandardError "$prefix.version.err"
    $actual = (Get-Content "$prefix.version.out" -Raw -Encoding utf8).Trim()
    if ($process.ExitCode -ne 0 -or $actual -cne "DiskMap $Version") {
        throw "程序版本不符 / Unexpected program version $actual"
    }
    # ASCII 无 BOM，模拟 MCP 客户端的输入输出管道，并在读到 EOF 后等待退出。
    '{"jsonrpc":"2.0","id":1,"method":"ping"}' | Set-Content "$prefix.mcp.in" -Encoding ascii
    $process = Start-Process -FilePath $Program -ArgumentList 'mcp --no-attach' -Wait -PassThru -NoNewWindow `
        -RedirectStandardInput "$prefix.mcp.in" -RedirectStandardOutput "$prefix.mcp.out" `
        -RedirectStandardError "$prefix.mcp.err"
    $lines = @(Get-Content "$prefix.mcp.out" -Encoding utf8 | Where-Object { $_.Trim() })
    if ($process.ExitCode -ne 0 -or $lines.Count -ne 1) {
        throw "MCP 输出数量不符 / Unexpected MCP output $Program"
    }
    $reply = $lines[0] | ConvertFrom-Json
    if ($reply.jsonrpc -cne '2.0' -or $reply.id -ne 1 -or
        ($reply.result | ConvertTo-Json -Compress) -cne '{}' -or $null -ne $reply.error) {
        throw "MCP 回复不符 / Unexpected MCP reply $($lines[0])"
    }
    Write-Host "自检通过 / Package self-test passed $Program"
}

function Test-Package([string]$Directory, [string]$Label) {
    Test-Program (Join-Path $Directory 'DiskMap.exe') "$Label-desktop"
    Test-Program (Join-Path $Directory 'diskmap-cli.exe') "$Label-cli"
}

$nsis = Join-Path $out "DiskMap-$Version-windows-x64-setup.exe"
$nsisDir = Join-Path $env:LOCALAPPDATA 'Programs/DiskMap'
$nsisUninstall = Join-Path $nsisDir 'uninstall.exe'
try {
    # 没有 /D，验证安装器的默认当前用户目录。
    Invoke-CheckedProcess $nsis '/S'
    Test-Package $nsisDir 'nsis'
} finally {
    if (Test-Path $nsisUninstall) { Invoke-CheckedProcess $nsisUninstall '/S' }
    # 不传 _?=，沿用用户卸载时的临时副本流程，再等待真正的文件删除完成。
    Wait-NsisUninstall $nsisDir
}
Assert-PreservedData

$msi = Join-Path $out "DiskMap-$Version-windows-x64.msi"
$msiDir = Join-Path $env:ProgramFiles 'DiskMap'
$msiInstalled = $false
try {
    # 没有 INSTALLDIR，验证 MSI 未复用先前 NSIS 的 HKCU 路径。
    Invoke-CheckedProcess 'msiexec.exe' ('/i "{0}" /qn /norestart /L*v "{1}"' -f $msi, (Join-Path $work 'msi-install.log'))
    $msiInstalled = $true
    Test-Package $msiDir 'msi'
} finally {
    if ($msiInstalled) {
        Invoke-CheckedProcess 'msiexec.exe' ('/x "{0}" /qn /norestart /L*v "{1}"' -f $msi, (Join-Path $work 'msi-uninstall.log'))
    }
}
if ((Test-Path (Join-Path $msiDir 'DiskMap.exe')) -or
    (Test-Path (Join-Path $msiDir 'diskmap-cli.exe'))) {
    throw 'MSI 卸载残留程序 / MSI left installed executables'
}
Assert-PreservedData

$portableDir = Join-Path $work 'portable unpack'
Expand-Archive -LiteralPath (Join-Path $out "DiskMap-$Version-windows-x64-portable.zip") -DestinationPath $portableDir
Test-Package $portableDir 'portable'
$cliDir = Join-Path $work 'cli unpack'
Expand-Archive -LiteralPath (Join-Path $out "diskmap-cli-$Version-windows-x64.zip") -DestinationPath $cliDir
Test-Program (Join-Path $cliDir 'diskmap-cli.exe') 'standalone-cli'
