<#
.SYNOPSIS
  Dot-source helper: reads brand.toml (the product name lives only there) and renders *.in templates.

.DESCRIPTION
  `. (Join-Path $PSScriptRoot 'brand.ps1')` sets $Brand, a hashtable of every `key = "string"` in
  brand.toml's [product], [identity] and [stable] sections ($Brand.display_name, $Brand.binary, …;
  $env:BRAND_FILE, relative to the workspace root, picks another file), and defines
  Expand-BrandTemplate IN OUT EXTRA, which fills {{key}} placeholders (an unknown one is an error).
#>
$BrandRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$BrandFile = if ($env:BRAND_FILE) { Join-Path $BrandRoot $env:BRAND_FILE } else { Join-Path $BrandRoot 'brand.toml' }
$Brand = @{}
$section = ''
foreach ($line in Get-Content $BrandFile) {
  $l = $line.Trim()
  if ($l -match '^\[(.+)\]') { $section = $Matches[1].Trim(); continue }
  if ($section -notin 'product', 'identity', 'stable') { continue }
  if ($l -match '^([A-Za-z0-9_]+)\s*=\s*"((?:[^"\\]|\\.)*)"') {
    $Brand[$Matches[1]] = $Matches[2] -replace '\\(.)', '$1'
  }
}
foreach ($k in 'display_name', 'binary', 'cli_binary', 'app_id', 'env_prefix', 'vendor') {
  if (-not $Brand[$k]) { throw "$BrandFile has no $k" }
}
$Brand['app'] = $Brand['display_name']

function Expand-BrandTemplate([string] $In, [string] $Out, [hashtable] $Extra = @{}) {
  $vars = $Brand.Clone()
  foreach ($k in $Extra.Keys) { $vars[$k] = $Extra[$k] }
  $text = Get-Content -Raw $In
  $text = [regex]::Replace($text, '\{\{([a-z0-9_]+)\}\}', {
      param($m)
      $k = $m.Groups[1].Value
      if (-not $vars.ContainsKey($k)) { throw "${In}: unknown placeholder {{$k}}" }
      $vars[$k]
    })
  Set-Content -Path $Out -Value $text -Encoding utf8 -NoNewline
}
