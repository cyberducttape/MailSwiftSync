# MailSwiftSync installer for Windows.
#
#   irm https://raw.githubusercontent.com/cyberducttape/MailSwiftSync/main/install.ps1 | iex
#
# Downloads the newest published MailSwiftSync build, checks its SHA-256
# checksum, installs it for the current user (no administrator rights), adds
# it to your PATH and Start menu, and offers to install the qualified imapsync
# migration engine.
#
# To pass options, download the script first:
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/cyberducttape/MailSwiftSync/main/install.ps1))) -NoEngine
#
#   -InstallDir DIR   install into DIR (default: %LOCALAPPDATA%\Programs\MailSwiftSync)
#   -Version TAG      install a specific release tag instead of the newest
#   -NoEngine         do not offer to install imapsync
#   -NoShortcut       do not add a Start menu shortcut
#   -Yes              answer yes to every question
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\MailSwiftSync'),
    [string]$Version = '',
    [switch]$NoEngine,
    [switch]$NoShortcut,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Repo = 'cyberducttape/MailSwiftSync'
$Target = 'x86_64-pc-windows-msvc'
$ArchiveName = "mailswiftsync-$Target.zip"

function Write-Step([string]$Message) { Write-Host "`n==> $Message" -ForegroundColor Cyan }

function Confirm-Step([string]$Question) {
    if ($Yes) { return $true }
    $answer = Read-Host "$Question [Y/n]"
    return ($answer -eq '' -or $answer -match '^(y|yes)$')
}

function Find-AssetUrl([string]$Asset) {
    $headers = @{ 'User-Agent' = 'mailswiftsync-installer' }
    # Optional; avoids the anonymous API rate limit on shared CI runners.
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $($env:GITHUB_TOKEN)" }
    if ($Version) {
        $releases = @(Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$Repo/releases/tags/$Version")
    } else {
        $releases = @(Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$Repo/releases?per_page=30")
    }
    foreach ($release in $releases) {
        # Never install from a draft (visible to authenticated callers);
        # prereleases stay eligible because preview builds are published so.
        if ($release.draft -ne $false) { continue }
        foreach ($item in $release.assets) {
            if ($item.name -eq $Asset) {
                return [pscustomobject]@{ Url = $item.browser_download_url; Tag = $release.tag_name }
            }
        }
    }
    return $null
}

Write-Host 'MailSwiftSync installer'
if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'MailSwiftSync needs 64-bit Windows.'
}

$asset = Find-AssetUrl $ArchiveName
if (-not $asset) {
    throw "No published MailSwiftSync build for Windows was found at https://github.com/$Repo/releases."
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("mailswiftsync-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Force $work | Out-Null
try {
    Write-Step "Downloading $ArchiveName ($($asset.Tag))"
    Write-Host "    $($asset.Url)"
    $archive = Join-Path $work $ArchiveName
    Invoke-WebRequest -UseBasicParsing -Uri $asset.Url -OutFile $archive
    try {
        $checksumText = (Invoke-WebRequest -UseBasicParsing -Uri "$($asset.Url).sha256").Content
    } catch {
        throw "The release has no checksum for $ArchiveName; refusing to install it."
    }
    if ($checksumText -is [byte[]]) { $checksumText = [Text.Encoding]::ASCII.GetString($checksumText) }
    $expected = ($checksumText.Trim() -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    if (-not $expected -or $expected -ne $actual) {
        throw "Checksum mismatch for $ArchiveName (expected $expected, got $actual); nothing was installed."
    }
    Write-Host "    SHA-256 verified: $actual"

    $bundle = Join-Path $work 'bundle'
    Expand-Archive -Path $archive -DestinationPath $bundle -Force
    $binary = Get-ChildItem -Path $bundle -Recurse -File |
        Where-Object { $_.Name -in @("mailswiftsync-$Target.exe", 'mailswiftsync.exe') } |
        Select-Object -First 1
    if (-not $binary) { throw 'The archive does not contain the MailSwiftSync program.' }

    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    $exe = Join-Path $InstallDir 'mailswiftsync.exe'
    if (Test-Path -LiteralPath $exe -PathType Container) {
        throw "$exe is a folder; move it away or choose another -InstallDir."
    }
    # Stage beside the target, verify, then swap. Windows lets a running
    # program be renamed but not overwritten, so the old copy is moved
    # aside first and restored if the new one cannot be put in place.
    $staged = "$exe.new"
    $previous = "$exe.old"
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $binary.FullName -Destination $staged
    $sourceHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary.FullName).Hash
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $staged).Hash -ne $sourceHash) {
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        throw 'The staged copy of MailSwiftSync does not match the download; nothing was changed.'
    }
    Remove-Item -LiteralPath $previous -Force -ErrorAction SilentlyContinue
    $hadPrevious = Test-Path -LiteralPath $exe -PathType Leaf
    if ($hadPrevious) {
        try {
            Move-Item -LiteralPath $exe -Destination $previous -Force
        } catch {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
            throw "Could not move the installed MailSwiftSync aside ($($_.Exception.Message)); the existing installation was left unchanged."
        }
    }
    try {
        Move-Item -LiteralPath $staged -Destination $exe -Force
    } catch {
        if ($hadPrevious) { Move-Item -LiteralPath $previous -Destination $exe -Force -ErrorAction SilentlyContinue }
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        throw "Could not install the new MailSwiftSync ($($_.Exception.Message)); the previous version was restored."
    }
    # A still-running old version keeps its file locked; it is removed on
    # the next install instead.
    Remove-Item -LiteralPath $previous -Force -ErrorAction SilentlyContinue
    foreach ($document in @('README.md', 'LICENSE', 'OFL-IBM-Plex.txt')) {
        $path = Join-Path $bundle $document
        if (Test-Path $path) { Copy-Item -Force $path $InstallDir }
    }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
Write-Step "Installed MailSwiftSync $($asset.Tag) to $InstallDir"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ })
if ($entries -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $InstallDir) -join ';'), 'User')
    $env:Path = "$env:Path;$InstallDir"
    Write-Host "    Added $InstallDir to your PATH (new terminals pick it up)."
}

if (-not $NoShortcut) {
    $programs = [Environment]::GetFolderPath('Programs')
    $shortcutPath = Join-Path $programs 'MailSwiftSync.lnk'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $exe
    $shortcut.WorkingDirectory = $InstallDir
    $shortcut.Description = 'Plan, run, and verify IMAP mailbox migrations'
    # The program is a console application; keep its console out of the way.
    $shortcut.WindowStyle = 7
    $shortcut.Save()
    Write-Host '    Added MailSwiftSync to the Start menu.'
}

if (-not $NoEngine) {
    Write-Step 'Checking the migration engine'
    # An older build may open its window for unknown arguments; give the
    # help probe a deadline instead of waiting for that window to close.
    $helpFile = Join-Path ([IO.Path]::GetTempPath()) ("mailswiftsync-help-" + [Guid]::NewGuid() + '.txt')
    $probe = Start-Process -FilePath $exe -ArgumentList '--help' -NoNewWindow -PassThru -RedirectStandardOutput $helpFile
    if (-not $probe.WaitForExit(15000)) { $probe.Kill() }
    $help = if (Test-Path $helpFile) { Get-Content -Raw $helpFile } else { '' }
    Remove-Item -Force $helpFile -ErrorAction SilentlyContinue
    # The installed build resolves the engine as the app does (saved engine
    # path, then PATH), so an engine installed earlier is not offered again.
    $engineQualified = $false
    if ($help -match 'install-engine') {
        try {
            $report = (& $exe doctor --json 2>$null | Out-String) | ConvertFrom-Json
            $engineQualified = @($report.checks | Where-Object { $_.name -eq 'imapsync executable' -and $_.status -eq 'qualified' }).Count -gt 0
        } catch { $engineQualified = $false }
    }
    if ($engineQualified) {
        Write-Host '    imapsync 2.314 is already installed.'
    } elseif ($help -notmatch 'install-engine') {
        Write-Warning 'This MailSwiftSync build cannot install the engine itself; install imapsync 2.314 from https://imapsync.lamiral.info/#install.'
    } elseif (Confirm-Step 'Install imapsync 2.314, the engine that copies the mail?') {
        & $exe install-engine --yes
        if ($LASTEXITCODE -ne 0) {
            Write-Warning 'The engine was not installed. Retry later with: mailswiftsync install-engine'
        }
    } else {
        Write-Host '    Skipped. Install it later with: mailswiftsync install-engine'
    }
}

Write-Step 'Done'
Write-Host 'Start MailSwiftSync from the Start menu, or run:  mailswiftsync'
Write-Host 'Check your setup at any time with:  mailswiftsync doctor'
