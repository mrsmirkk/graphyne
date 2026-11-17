# Builds and packages a portable Windows release of Graphyne into dist\.
# Usage:  powershell -ExecutionPolicy Bypass -File packaging\package-windows.ps1 [-SkipBuild]
param([switch]$SkipBuild)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

if (-not $SkipBuild) {
    # LuaJIT's msvcbuild.bat runs the helper tools it just built from the current folder;
    # this variable (set by some shells / agents) forbids that.
    Remove-Item Env:NoDefaultCurrentDirectoryInExePath -ErrorAction SilentlyContinue
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
}

$meta = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
$version = ($meta.packages | Where-Object { $_.name -eq "graphyne" }).version
$name = "Graphyne-$version-windows-x64"
$dist = Join-Path $root "dist"
$out = Join-Path $dist $name
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force $out | Out-Null

Copy-Item (Join-Path $root "target\release\graphyne.exe") $out
Copy-Item (Join-Path $root "packaging\README.md") $out
Copy-Item (Join-Path $root "LICENSE") $out

# Example extensions → plugins\ next to graphyne.exe. Grammar projects (grammar\Cargo.toml)
# are built and their library copied in as <extension>.dll.
$plugins = Join-Path $out "plugins"
New-Item -ItemType Directory -Force $plugins | Out-Null
Copy-Item (Join-Path $root "extensions\README.md") $plugins
foreach ($ext in Get-ChildItem (Join-Path $root "extensions") -Directory) {
    $dest = Join-Path $plugins $ext.Name
    Copy-Item -Recurse $ext.FullName $dest
    Remove-Item -Recurse -Force (Join-Path $dest "grammar") -ErrorAction SilentlyContinue
    $grammar = Join-Path $ext.FullName "grammar"
    if (Test-Path (Join-Path $grammar "Cargo.toml")) {
        Push-Location $grammar
        cargo build --release
        $ok = $LASTEXITCODE -eq 0
        Pop-Location
        if (-not $ok) { throw "grammar build failed: $($ext.Name)" }
        $dll = Get-ChildItem (Join-Path $grammar "target\release\*.dll") | Select-Object -First 1
        Copy-Item $dll.FullName (Join-Path $dest "$($ext.Name).dll")
    }
}

# Third-party notices: icon licences + every crate linked into the Windows build.
$notices = New-Object System.Collections.Generic.List[string]
$notices.Add("# Third-party notices")
$notices.Add("")
$notices.Add("Graphyne $version is MIT-licensed (see LICENSE) and bundles the following third-party components.")
$notices.Add("")
$notices.Add("## UI icons: Lucide (ISC)")
$notices.Add("")
$notices.Add((Get-Content (Join-Path $root "assets\icons\NOTICE.md") -Raw -Encoding UTF8))
$notices.Add("## File icons: Material Icon Theme (MIT)")
$notices.Add("")
$notices.Add('```')
$notices.Add((Get-Content (Join-Path $root "assets\file-icons\LICENSE") -Raw -Encoding UTF8).Trim())
$notices.Add('```')
$notices.Add("")
$notices.Add("## SQLite (public domain)")
$notices.Add("")
$notices.Add("The SQLite engine (https://sqlite.org, public domain) is compiled in through the rusqlite and")
$notices.Add("libsqlite3-sys crates for the Database panel.")
$notices.Add("")
$notices.Add("## Example extensions")
$notices.Add("")
$notices.Add("plugins\zig: grammar library and highlights query from tree-sitter-zig 1.1.2 (MIT,")
$notices.Add("https://github.com/tree-sitter-grammars/tree-sitter-zig). plugins\nord-theme: colours after")
$notices.Add("the Nord palette (MIT, https://www.nordtheme.com).")
$notices.Add("")
$notices.Add("## Rust crates")
$notices.Add("")
$notices.Add("Dependencies of the Windows build, including build-time crates (proc-macros, build scripts).")
$notices.Add("Licence texts are available in each crate's source on https://crates.io.")
$notices.Add("MPL-2.0 components (nucleo, nucleo-matcher, option-ext) are used unmodified; their source code")
$notices.Add("is available at https://crates.io and in their upstream repositories.")
$notices.Add("gpui (Apache-2.0, Zed Industries) is used with small local changes to its Windows platform code;")
$notices.Add("they are listed in third_party/gpui/GRAPHYNE-CHANGES.md of the Graphyne sources.")
$notices.Add("")
$crates = cargo tree -p graphyne -e normal,build --target x86_64-pc-windows-msvc --prefix none --format "{p}|{l}" |
    Where-Object { $_ -notmatch "\(\*\)" -and $_ -match "\|" } |
    ForEach-Object { ($_ -replace " \(proc-macro\)", "") -replace " \(D:.*?\)", "" } |
    Sort-Object -Unique
$notices.Add("| Crate | Licence |")
$notices.Add("| --- | --- |")
foreach ($c in $crates) {
    $parts = $c.Split("|")
    $notices.Add("| $($parts[0].Trim()) | $($parts[1].Trim()) |")
}
[System.IO.File]::WriteAllLines((Join-Path $out "THIRD-PARTY-NOTICES.md"), $notices, (New-Object System.Text.UTF8Encoding $false))

$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $out "*") -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Set-Content -Path "$zip.sha256" -Value "$hash  $name.zip" -Encoding ascii

$exe = Get-Item (Join-Path $out "graphyne.exe")
Write-Output ("Packaged {0}" -f $zip)
Write-Output ("  graphyne.exe {0:N1} MB, zip {1:N1} MB, {2} crates listed" -f ($exe.Length / 1MB), ((Get-Item $zip).Length / 1MB), $crates.Count)
Write-Output ("  sha256 {0}" -f $hash)
