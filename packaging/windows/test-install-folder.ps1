<#
  Exercise the install-folder searches and actions from lightcraft.wxs with real MSI AppSearch.
  Requires Windows and WiX 5.0.2. No application binaries, administrator rights, or real
  LightCraft installation are needed: a per-user probe writes only to a unique test registry key.
  Run: pwsh packaging/windows/test-install-folder.ps1 [-Arch x86|x64|arm64] [-Wix path/to/wix.exe]
#>
param(
  [ValidateSet('x86', 'x64', 'arm64')][string] $Arch = 'x64',
  [string] $Wix = 'wix'
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$id = [Guid]::NewGuid().ToString('N')
$root = Join-Path ([IO.Path]::GetTempPath()) "lightcraft-install-folder-$id"
$testsKey = 'Software\LightCraftInstallerTests'
$key = "$testsKey\$id"
$product = [Guid]::NewGuid().ToString('B')
$upgrade = [Guid]::NewGuid().ToString('B')
$view = if ($Arch -eq 'x86') { [Microsoft.Win32.RegistryView]::Registry32 } else { [Microsoft.Win32.RegistryView]::Registry64 }
$registry = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
$installed = $false
$passed = $false
New-Item -ItemType Directory -Path $root | Out-Null

function Invoke-Msi([string[]] $Arguments, [string] $Log, [switch] $AllowAbsent) {
  $p = Start-Process msiexec.exe -ArgumentList ($Arguments + @('/qn', '/norestart', '/l*v', "`"$Log`"")) -Wait -PassThru
  if ($p.ExitCode -ne 0 -and -not ($AllowAbsent -and $p.ExitCode -eq 1605)) {
    Get-Content $Log -Tail 40
    throw "msiexec exited $($p.ExitCode)"
  }
}

function Set-TestValue([string] $Subkey, [string] $Name, [string] $Value) {
  $k = $registry.CreateSubKey("$key\$Subkey")
  try { $k.SetValue($Name, $Value, [Microsoft.Win32.RegistryValueKind]::String) } finally { $k.Dispose() }
}

try {
  [xml] $source = Get-Content (Join-Path $PSScriptRoot 'lightcraft.wxs') -Raw
  $ns = [Xml.XmlNamespaceManager]::new($source.NameTable)
  $ns.AddNamespace('w', 'http://wixtoolset.org/schemas/v4/wxs')
  # Copy the real search/action authoring, changing only registry roots/keys for isolation.
  # Do not reimplement folder resolution here: removing the fallback must fail the legacy case.
  [xml] $scope = Get-Content (Join-Path $PSScriptRoot 'install-policy.wxi') -Raw
  $scopeNs = [Xml.XmlNamespaceManager]::new($scope.NameTable)
  $scopeNs.AddNamespace('w', 'http://wixtoolset.org/schemas/v4/wxs')
  $nodes = $scope.SelectNodes('/w:Include/w:Property | /w:Include/w:SetProperty[@Sequence="both"]', $scopeNs)
  $authoring = foreach ($node in $nodes) {
    $copy = $node.CloneNode($true)
    foreach ($search in $copy.SelectNodes('.//w:RegistrySearch', $ns)) {
      $search.SetAttribute('Root', 'HKCU')
      $search.SetAttribute('Key', "$key\$($search.GetAttribute('Key'))")
    }
    $copy.OuterXml
  }
  $directory = $source.SelectSingleNode('/w:Wix/w:Package/w:StandardDirectory[.//w:Directory[@Id="INSTALLFOLDER"]]', $ns).OuterXml
  $probe = @"
<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs">
  <Package Name="LightCraft folder search test $id" Manufacturer="LightCraft tests"
      Version="1.0.0" ProductCode="$product" UpgradeCode="$upgrade" Scope="perUserOrMachine">
    $($authoring -join "`n")
    $directory
    <Feature Id="Main"><ComponentRef Id="Result" /></Feature>
    <Component Id="Result" Directory="INSTALLFOLDER" Guid="*">
      <RegistryValue Root="HKCU" Key="$key\Result" Name="Folder" Value="[INSTALLFOLDER]" Type="string" KeyPath="yes" />
    </Component>
  </Package>
</Wix>
"@
  $wxs = Join-Path $root 'probe.wxs'
  $msi = Join-Path $root 'probe.msi'
  $probe | Set-Content $wxs -Encoding utf8
  & $Wix build $wxs -arch $Arch -o $msi
  if ($LASTEXITCODE -ne 0) { throw 'Could not build the folder-search probe' }

  $legacy = Join-Path $root 'legacy app café'
  $modern = Join-Path $root 'remembered app'
  $explicit = Join-Path $root 'explicit app'
  $emptied = Join-Path $root 'emptied app'
  New-Item -ItemType Directory -Path $legacy, $modern, $explicit, $emptied | Out-Null
  $legacyExe = Join-Path $legacy 'lightcraft.exe'
  [IO.File]::WriteAllText($legacyExe, 'test placeholder; never executed')
  $programFiles = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\ArtCraft'
  $cases = @(
    @{ Name = 'fresh'; Expected = (Join-Path $programFiles 'LightCraft') },
    @{ Name = 'legacy-app-paths'; Legacy = $legacyExe; Expected = $legacy },
    # A stale unquoted path with a space used to resolve to the parent of the text before the space.
    @{ Name = 'legacy-folder-deleted'; Legacy = (Join-Path $root 'deleted app\lightcraft.exe'); Expected = (Join-Path $programFiles 'LightCraft') },
    @{ Name = 'legacy-exe-deleted'; Legacy = (Join-Path $emptied 'lightcraft.exe'); Expected = (Join-Path $programFiles 'LightCraft') },
    @{ Name = 'remembered-folder'; Modern = $true; Expected = $modern },
    @{ Name = 'remembered-beats-legacy'; Modern = $true; Legacy = $legacyExe; Expected = $modern },
    @{ Name = 'explicit-beats-legacy'; Legacy = $legacyExe; Explicit = $true; Expected = $explicit },
    @{ Name = 'explicit-beats-both'; Modern = $true; Legacy = $legacyExe; Explicit = $true; Expected = $explicit }
  )
  foreach ($case in $cases) {
    $registry.DeleteSubKeyTree($key, $false)
    if ($case.Legacy) { Set-TestValue 'Software\Microsoft\Windows\CurrentVersion\App Paths\lightcraft.exe' '' $case.Legacy }
    if ($case.Modern) { Set-TestValue 'Software\LightCraft' 'InstallFolder' "$modern\" }
    $arguments = @('/i', "`"$msi`"")
    if ($case.Explicit) { $arguments += "INSTALLFOLDER=`"$explicit`"" }
    # Also attempt cleanup if an installation only partially completes.
    $installed = $true
    Invoke-Msi $arguments (Join-Path $root "$($case.Name)-install.log")
    $result = $registry.OpenSubKey("$key\Result")
    if (-not $result) { throw "$($case.Name): no result recorded" }
    try { $actual = [string] $result.GetValue('Folder') } finally { $result.Dispose() }
    if ($actual.TrimEnd('\') -ine $case.Expected.TrimEnd('\')) {
      throw "$($case.Name): expected '$($case.Expected)', got '$actual'"
    }
    Invoke-Msi @('/x', $product) (Join-Path $root "$($case.Name)-uninstall.log")
    $installed = $false
    Write-Output "ok $Arch $($case.Name)"
  }
  $passed = $true
} finally {
  try {
    if ($installed) { Invoke-Msi @('/x', $product) (Join-Path $root 'cleanup.log') -AllowAbsent }
  } finally {
    $registry.DeleteSubKeyTree($key, $false)
    $tests = $registry.OpenSubKey($testsKey)
    if ($tests) {
      $empty = $tests.SubKeyCount -eq 0 -and $tests.ValueCount -eq 0
      $tests.Dispose()
      if ($empty) { $registry.DeleteSubKey($testsKey, $false) }
    }
    $registry.Dispose()
    # Keep logs on failure; they explain MSI failures that otherwise only report an exit code.
    if ($passed) { Remove-Item -Recurse -Force $root } else { Write-Output "Test artifacts retained: $root" }
  }
}
