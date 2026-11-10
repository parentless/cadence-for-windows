param(
    [ValidateSet('debug', 'release')][string]$Profile = 'release',
    [ValidatePattern('^[a-z0-9][a-z0-9-]{0,63}$')][string]$Label
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    $version = ($metadata.packages | Where-Object name -eq 'cadence').version
    $binary = Join-Path $metadata.target_directory "x86_64-pc-windows-msvc/$Profile/cadence.exe"
    if (!(Test-Path -LiteralPath $binary)) {
        throw "Build first: cargo build --release --locked --target x86_64-pc-windows-msvc"
    }
    $name = "cadence-$version-windows-x86_64"
    if ($Profile -eq 'debug') { $name += '-debug' }
    if ($Label) { $name += "-$Label" }
    $staging = Join-Path $root "dist/$name"
    if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    Copy-Item -LiteralPath $binary -Destination "$staging/Cadence.exe" -Force
    Copy-Item -LiteralPath LICENSE, THIRD_PARTY_NOTICES.md -Destination $staging -Force
    Copy-Item -LiteralPath vendor/gpui-pre-windows/LICENSE-APACHE -Destination "$staging/GPUI-WINDOWS-LICENSE.txt" -Force
    Copy-Item -LiteralPath assets/fonts/LICENSE.txt -Destination "$staging/INTER-LICENSE.txt" -Force
    Copy-Item -LiteralPath docs/WINDOWS.md -Destination "$staging/README.md" -Force
    Compress-Archive -Path "$staging/*" -DestinationPath "dist/$name.zip" -Force
    # The updater downloads the standalone executable; the zip includes notices
    # and instructions for first-time installs.
    Copy-Item -LiteralPath $binary -Destination "dist/$name.exe" -Force
    # Keep the versioned alias for installed clients predating the portable name.
    $executables = @("dist/$name.exe")
    if ($Profile -eq 'release' -and !$Label) {
        Copy-Item -LiteralPath $binary -Destination 'dist/Cadence.exe' -Force
        $executables += 'dist/Cadence.exe'
    }
    foreach ($file in ($executables + @("dist/$name.zip"))) {
        $hash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $(Split-Path -Leaf $file)`n" | Set-Content -LiteralPath "$file.sha256" -Encoding ascii -NoNewline
    }
    # Only prune superseded Windows packages after the replacement and its
    # checksums have been created successfully. Leave other dist artifacts alone.
    $keep = @("$name.zip", "$name.zip.sha256", "$name.exe", "$name.exe.sha256")
    Get-ChildItem -LiteralPath (Join-Path $root 'dist') | Where-Object {
        $_.Name -like 'cadence-*-windows-x86_64*' -and $_.Name -notin $keep
    } | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force }
    Write-Host "Packaged dist/$name.zip and dist/$name.exe; removed superseded Windows builds"
} finally { Pop-Location }
