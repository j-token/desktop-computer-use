$ErrorActionPreference = 'Stop'

$ExtensionsRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$DistDir = Join-Path $ExtensionsRoot 'dist'
$Uuid = 'desktop-computer-use@local'
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

function Package-Variant([string]$Variant) {
    $SourceDir = Join-Path $ExtensionsRoot ("gnome-{0}" -f $Variant)
    $StageDir = Join-Path $DistDir $Uuid
    $Archive = Join-Path $DistDir ("desktop-computer-use-gnome-{0}.zip" -f $Variant)
    $ResolvedDist = [IO.Path]::GetFullPath($DistDir).TrimEnd([IO.Path]::DirectorySeparatorChar)
    $ResolvedStage = [IO.Path]::GetFullPath($StageDir)
    if (-not $ResolvedStage.StartsWith($ResolvedDist + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing package staging directory outside extensions/dist: $ResolvedStage"
    }
    if (-not (Test-Path -LiteralPath $SourceDir -PathType Container)) {
        throw "Missing extension source: $SourceDir"
    }

    if (Test-Path -LiteralPath $StageDir) {
        Remove-Item -LiteralPath $StageDir -Recurse -Force
    }
    if (Test-Path -LiteralPath $Archive) {
        Remove-Item -LiteralPath $Archive -Force
    }
    New-Item -ItemType Directory -Force -Path $StageDir | Out-Null
    Copy-Item -Path (Join-Path $SourceDir '*') -Destination $StageDir -Recurse -Force

    $glibCompile = Get-Command glib-compile-schemas -ErrorAction SilentlyContinue
    if ($null -ne $glibCompile) {
        & $glibCompile.Source (Join-Path $StageDir 'schemas')
    }
    Compress-Archive -Path (Join-Path $StageDir '*') -DestinationPath $Archive -CompressionLevel Optimal
    Remove-Item -LiteralPath $StageDir -Recurse -Force
    Write-Output $Archive
}

Package-Variant 'modern'
Package-Variant 'legacy'
