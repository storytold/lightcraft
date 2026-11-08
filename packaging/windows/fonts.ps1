# Font setup for local packaging; dot-sourced by package.ps1.
function Initialize-CraftFonts([string] $WorkspaceRoot) {
  $fontDir = $env:CRAFT_FONTS_DIR
  if (-not $fontDir) { $fontDir = Join-Path $WorkspaceRoot '../craft-fonts' }
  if (-not [IO.Path]::IsPathRooted($fontDir)) { $fontDir = Join-Path $WorkspaceRoot $fontDir }

  if (-not $env:CRAFT_FONTS_DIR -and -not (Test-Path -LiteralPath $fontDir)) {
    # Same revision as release.yml and packaging/fonts.sh. Existing checkouts stay untouched.
    $revision = 'b076417b5880bd4154bd22f00695b1d28c6bea83'
    Write-Output "==> downloading craft-fonts ($revision) to $fontDir"
    & git clone --no-checkout https://github.com/storytold/craft-fonts $fontDir
    if ($LASTEXITCODE -ne 0) { throw 'Could not download craft-fonts; provide a complete checkout with CRAFT_FONTS_DIR' }
    & git -C $fontDir checkout --detach $revision
    if ($LASTEXITCODE -ne 0) { throw 'Could not check out craft-fonts; provide a complete checkout with CRAFT_FONTS_DIR' }
  }

  $manifest = Join-Path $fontDir 'fonts/manifest.txt'
  if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) {
    throw "Missing $manifest; set CRAFT_FONTS_DIR to a complete craft-fonts checkout"
  }
  $env:CRAFT_FONTS_DIR = (Resolve-Path -LiteralPath $fontDir).Path
  $env:CRAFT_FONTS_REQUIRED = '1'
  Write-Output "==> embedding fonts from $env:CRAFT_FONTS_DIR"
}
