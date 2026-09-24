param(
    [Parameter(Mandatory = $true)]
    [string]$Version
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$utf8 = New-Object System.Text.UTF8Encoding($false)
$backups = @{}

function Invoke-Git {
    param([string[]]$GitArgs)
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & git @GitArgs 2>&1 | ForEach-Object { "$_" }
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
    return @{ Code = $code; Output = ($output -join "`n").Trim() }
}

function Run-Git {
    param([string[]]$GitArgs)
    $result = Invoke-Git $GitArgs
    if ($result.Code -ne 0) { throw "git $($GitArgs -join ' ') failed: $($result.Output)" }
    return $result.Output
}

function Set-VersionIn {
    param([string]$RelPath, [string]$Pattern, [int]$Expected, [string]$Old, [string]$New)
    $path = Join-Path $root $RelPath
    $text = [System.IO.File]::ReadAllText($path, $utf8)
    $regex = New-Object System.Text.RegularExpressions.Regex($Pattern.Replace('{OLD}', [regex]::Escape($Old)), 'Multiline')
    $count = $regex.Matches($text).Count
    if ($count -ne $Expected) { throw "$RelPath`: expected $Expected version match(es) for $Old, found $count" }
    $backups[$path] = $text
    $updated = $regex.Replace($text, '${1}' + $New + '${2}')
    [System.IO.File]::WriteAllText($path, $updated, $utf8)
}

function Restore-Backups {
    foreach ($path in $backups.Keys) {
        try { [System.IO.File]::WriteAllText($path, $backups[$path], $utf8) } catch { Write-Host "Could not restore $path" -ForegroundColor Red }
    }
}

try {
    Set-Location $root

    if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
        throw "Version must be MAJOR.MINOR.PATCH, for example 1.0.1"
    }
    $tag = "v$Version"

    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { throw 'git not found in PATH.' }

    $branch = Run-Git @('rev-parse', '--abbrev-ref', 'HEAD')
    if ($branch -ne 'main') { throw "Releases are made from main, current branch is $branch." }

    $files = @('package.json', 'package-lock.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock')
    $dirty = Run-Git (@('status', '--porcelain', '--') + $files)
    if ($dirty) { throw "Version files have uncommitted changes:`n$dirty" }

    if ((Invoke-Git @('rev-parse', '-q', '--verify', "refs/tags/$tag")).Code -eq 0) { throw "Tag $tag already exists locally." }
    $remoteTag = Run-Git @('ls-remote', '--tags', 'origin', "refs/tags/$tag")
    if ($remoteTag) { throw "Tag $tag already exists on origin." }

    $current = (Get-Content (Join-Path $root 'package.json') -Raw | ConvertFrom-Json).version
    if (-not $current) { throw 'Could not read the current version from package.json.' }
    if ($current -eq $Version) { throw "Version is already $Version." }

    Write-Host "Synapse $current -> $Version" -ForegroundColor Cyan

    Set-VersionIn 'package.json' '^(\s*"version": "){OLD}(",?)(?=\r?$)' 1 $current $Version
    Set-VersionIn 'package-lock.json' '("name": "synapse",\r?\n\s*"version": "){OLD}(")' 2 $current $Version
    Set-VersionIn 'src-tauri/tauri.conf.json' '^(\s*"version": "){OLD}(",?)(?=\r?$)' 1 $current $Version
    Set-VersionIn 'src-tauri/Cargo.toml' '^(version = "){OLD}(")' 1 $current $Version
    Set-VersionIn 'src-tauri/Cargo.lock' '(name = "synapse"\r?\nversion = "){OLD}(")' 1 $current $Version

    Run-Git (@('add', '--') + $files) | Out-Null
    Run-Git @('commit', '-m', "Release $tag", '--', 'package.json', 'package-lock.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock') | Out-Null
    $backups.Clear()
    Run-Git @('tag', '-a', $tag, '-m', "Synapse $Version") | Out-Null
    Run-Git @('push', 'origin', 'main') | Out-Null
    Run-Git @('push', 'origin', $tag) | Out-Null

    Write-Host "Released $tag. GitHub Actions is building the installers:" -ForegroundColor Green
    Write-Host "  https://github.com/Monayzera/Synapse/actions"
} catch {
    Restore-Backups
    Write-Host ""
    Write-Host "RELEASE ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
