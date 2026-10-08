param(
    [ValidateSet('Debug','Release')][string]$Configuration = 'Release',
    [ValidateSet('arm64-v8a','x86_64')][string]$Abi = 'arm64-v8a'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot
if (-not $env:ANDROID_HOME) { throw 'Set ANDROID_HOME to the Android SDK directory.' }
if (-not $env:JAVA_HOME) { throw 'Set JAVA_HOME to a Java 17 JDK directory.' }
$ndk = Join-Path $env:ANDROID_HOME 'ndk/28.2.13676358'
$bin = Join-Path $ndk 'toolchains/llvm/prebuilt/windows-x86_64/bin'
$target = if ($Abi -eq 'arm64-v8a') { 'aarch64-linux-android' } else { 'x86_64-linux-android' }
$key = 'CARGO_TARGET_' + $target.ToUpper().Replace('-','_') + '_LINKER'
[Environment]::SetEnvironmentVariable($key, (Join-Path $bin ($target + '29-clang.cmd')), 'Process')
Push-Location $repo
try {
    $args = @('build','--locked','-p','lightcraft-android','--target',$target)
    if ($Configuration -eq 'Release') { $args += '--release' }
    & cargo @args
    if ($LASTEXITCODE -ne 0) { throw 'Rust Android build failed.' }
    $profile = $Configuration.ToLower()
    $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    $library = Join-Path $targetDir "$target/$profile/liblightcraft_android.so"
    $dest = Join-Path $PSScriptRoot "app/src/main/jniLibs/$Abi"
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item -LiteralPath $library -Destination (Join-Path $dest 'liblightcraft_android.so')
    & (Join-Path $bin 'llvm-strip.exe') --strip-debug (Join-Path $dest 'liblightcraft_android.so')
    if ($LASTEXITCODE -ne 0) { throw 'Native debug stripping failed.' }
    & "$PSScriptRoot/gradlew.bat" -p $PSScriptRoot ":app:assemble$Configuration" "-PandroidAbi=$Abi" --console=plain
    if ($LASTEXITCODE -ne 0) { throw 'APK packaging failed.' }
    Write-Output (Join-Path $PSScriptRoot "app/build/outputs/apk/$profile/app-$profile.apk")
} finally { Pop-Location }
