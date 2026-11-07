param(
    [ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')]
    [string]$Target,
    [string]$Output = (Join-Path $PSScriptRoot '..\dist'),
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
if ($Help) {
    Write-Host '用法: .\scripts\package-windows.ps1 [-Target <Rust 目标>] [-Output <目录>]'
    Write-Host '目标支持 x86_64-pc-windows-msvc 和 aarch64-pc-windows-msvc；默认使用 Cargo 的默认目标。'
    exit 0
}
if ($env:OS -ne 'Windows_NT') { throw '请在 Windows 上运行此脚本。' }

$projectDir = Split-Path -Parent $PSScriptRoot
$nsis = Get-Command makensis.exe -ErrorAction SilentlyContinue
if ($nsis) { $nsisPath = $nsis.Source } else { $nsisPath = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe' }
if (-not (Test-Path -LiteralPath $nsisPath -PathType Leaf)) { throw '未找到 NSIS 的 makensis.exe，请安装 NSIS 并加入 PATH。' }
$manifest = Join-Path $projectDir 'Cargo.toml'
$metadataText = & cargo metadata --locked --no-deps --format-version 1 --manifest-path $manifest
if ($LASTEXITCODE -ne 0) { throw '无法读取 Cargo 项目信息。' }
$metadata = $metadataText | ConvertFrom-Json
$package = @($metadata.packages | Where-Object { $_.name -eq 'take-a-panel' })
if ($package.Count -ne 1) { throw '无法确定项目版本。' }
$version = $package[0].version

$cargoArgs = @('build', '--locked', '--release', '--bin', 'take-a-panel', '--manifest-path', $manifest, '--message-format=json-render-diagnostics')
if ($Target) { $cargoArgs += @('--target', $Target) }
$executable = $null
& cargo @cargoArgs | ForEach-Object {
    $message = $_ | ConvertFrom-Json
    if ($message.reason -eq 'compiler-message' -and $message.message.rendered) {
        [Console]::Error.Write($message.message.rendered)
    }
    if ($message.reason -eq 'compiler-artifact' -and $message.target.name -eq 'take-a-panel' -and $message.executable) {
        $executable = $message.executable
    }
}
if ($LASTEXITCODE -ne 0) { throw 'Cargo 构建失败。' }
if (-not $executable -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Cargo 未生成可执行文件。' }

# Read the PE machine field so the archive name reflects Cargo's actual target,
# including a default target configured outside this script.
$stream = [System.IO.File]::OpenRead($executable)
$reader = New-Object System.IO.BinaryReader($stream)
try {
    if ($reader.ReadUInt16() -ne 0x5a4d) { throw '构建产物不是 Windows 可执行文件。' }
    $stream.Position = 0x3c
    $peOffset = $reader.ReadUInt32()
    if ($peOffset -gt $stream.Length - 6) { throw 'Windows 可执行文件格式无效。' }
    $stream.Position = $peOffset
    if ($reader.ReadUInt32() -ne 0x00004550) { throw 'Windows 可执行文件格式无效。' }
    $architecture = switch ($reader.ReadUInt16()) {
        0x8664 { 'x86_64' }
        0xaa64 { 'arm64' }
        default { throw '不支持此 Windows 可执行文件的架构。' }
    }
} finally {
    $reader.Dispose()
}

$null = New-Item -ItemType Directory -Force -Path $Output
$outputDir = (Resolve-Path -LiteralPath $Output).Path
$stage = Join-Path $metadata.target_directory ("windows-package-" + [guid]::NewGuid().ToString('N'))
$archiveName = "Take-a-Panel-$version-windows-$architecture.zip"
try {
    $bundle = Join-Path $stage 'Take a Panel'
    $null = New-Item -ItemType Directory -Force -Path $bundle
    Copy-Item -LiteralPath $executable -Destination (Join-Path $bundle 'take-a-panel.exe')
    Copy-Item -LiteralPath (Join-Path $projectDir 'LICENSE') -Destination $bundle
    $stagedArchive = Join-Path $stage $archiveName
    Compress-Archive -LiteralPath $bundle -DestinationPath $stagedArchive
    $installerName = "Take-a-Panel-$version-windows-$architecture-setup.exe"
    $stagedInstaller = Join-Path $stage $installerName
    & $nsisPath "/DAPP_BINARY=$executable" "/DAPP_ICON=$(Join-Path $projectDir 'packaging\windows\AppIcon.ico')" "/DLICENSE_FILE=$(Join-Path $projectDir 'LICENSE')" "/DOUTPUT_FILE=$stagedInstaller" "/DAPP_VERSION=$version" (Join-Path $projectDir 'packaging\windows\installer.nsi')
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $stagedInstaller -PathType Leaf)) { throw 'NSIS 安装程序构建失败。' }
    Move-Item -LiteralPath $stagedArchive -Destination (Join-Path $outputDir $archiveName) -Force
    Move-Item -LiteralPath $stagedInstaller -Destination (Join-Path $outputDir $installerName) -Force
    Write-Host "`n压缩包: $(Join-Path $outputDir $archiveName)"
    Write-Host "安装程序: $(Join-Path $outputDir $installerName)"
} finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
}
