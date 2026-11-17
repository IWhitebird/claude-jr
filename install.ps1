# Installs claudejr (claude-jr, "Claude Junior") on Windows from the latest release:
#   irm https://claudejr.iwhitebird.com/install.ps1 | iex
# It goes to %LOCALAPPDATA%\Programs\claudejr (set CLAUDE_JR_BIN for another
# folder), which is added to your PATH. 64-bit Intel/AMD; on ARM it runs emulated.
$ErrorActionPreference = 'Stop'
$base = if ($env:CLAUDE_JR_RELEASES) { $env:CLAUDE_JR_RELEASES } else { 'https://github.com/IWhitebird/claude-jr/releases/latest/download' }
$dir = if ($env:CLAUDE_JR_BIN) { $env:CLAUDE_JR_BIN } else { Join-Path $env:LOCALAPPDATA 'Programs\claudejr' }
$name = 'claudejr-windows-x86_64.exe'

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("claudejr-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "downloading $name"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -UseBasicParsing "$base/$name" -OutFile (Join-Path $tmp 'claudejr.exe')
    Invoke-WebRequest -UseBasicParsing "$base/checksums.txt" -OutFile (Join-Path $tmp 'checksums.txt')

    $line = Get-Content (Join-Path $tmp 'checksums.txt') | Where-Object { $_ -match "[ *]$([regex]::Escape($name))$" } | Select-Object -First 1
    $want = if ($line) { ($line -split ' ')[0] } else { '' }
    $got = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp 'claudejr.exe')).Hash.ToLower()
    if (-not $want -or $want -ne $got) {
        throw "the download doesn't match its checksum; nothing was installed"
    }

    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $exe = Join-Path $dir 'claudejr.exe'
    # A copy that is running can't be written over, but it can be moved aside.
    if (Test-Path $exe) {
        Remove-Item -Force "$exe.old" -ErrorAction SilentlyContinue
        Move-Item -Force $exe "$exe.old"
    }
    Move-Item -Force (Join-Path $tmp 'claudejr.exe') $exe
    Write-Host "installed $(& $exe --version) to $exe"

    $path = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($path -split ';') -notcontains $dir) {
        [Environment]::SetEnvironmentVariable('Path', ($path.TrimEnd(';') + ';' + $dir).TrimStart(';'), 'User')
        Write-Host "added $dir to your PATH: open a new terminal, then run: claudejr"
    } else {
        Write-Host 'run: claudejr'
    }
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        Write-Host 'Git for Windows is recommended: claude-jr runs commands in its bash (PowerShell is used without it).'
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
