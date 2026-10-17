# Install the SAM 3 model for the app's Object and Describe masks on Windows
# (see docs/ai-masks.md; tools/install-sam3.sh is the macOS/Linux version). The desktop app
# offers to download the model itself when an AI mask first needs it; this is for developers.
#
# facebook/sam3 is gated: accept the SAM License at https://huggingface.co/facebook/sam3, wait
# for approval, then:   $env:HF_TOKEN = "hf_..." ; .\tools\install-sam3.ps1
param(
    [string]$Dir = "",
    [string]$Repo = "facebook/sam3",
    [switch]$Check
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot '..\packaging\windows\brand.ps1')
if (-not $Dir) {
    $envDir = [Environment]::GetEnvironmentVariable("$($Brand.env_prefix)_SAM3_DIR")
    $Dir = if ($envDir) { $envDir } else { Join-Path $env:APPDATA "$($Brand.settings_dir)\models\sam3" }
}
$Sha256 = "6d06f0a5f84e435071fe6603e61d0b4cc7b40e0d39d487cfd4d67d8cc11cc14a"
$Files = "config.json", "vocab.json", "merges.txt", "tokenizer.json", "tokenizer_config.json", "special_tokens_map.json", "processor_config.json"

function Test-Install {
    foreach ($f in "model.safetensors", "vocab.json", "merges.txt") {
        if (-not (Test-Path (Join-Path $Dir $f))) { throw "missing: $(Join-Path $Dir $f)" }
    }
    Write-Host "checking model.safetensors (SHA-256 of 3.4 GB)..."
    $got = (Get-FileHash (Join-Path $Dir "model.safetensors") -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $Sha256) { throw "model.safetensors does not match the official checkpoint (got $got)" }
    Write-Host "SAM 3 is installed in $Dir"
}

if ($Check) { Test-Install; exit 0 }

$Token = if ($env:HF_TOKEN) { $env:HF_TOKEN } else { $tf = Join-Path $HOME ".cache\huggingface\token"; if (Test-Path $tf) { (Get-Content $tf -Raw).Trim() } }
if (-not $Token -and $Repo -eq "facebook/sam3") {
    throw "No Hugging Face token. Accept the license at https://huggingface.co/facebook/sam3, then set HF_TOKEN or run 'hf auth login'."
}
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
$Headers = @{}
if ($Token) { $Headers["Authorization"] = "Bearer $Token" }
$Base = "https://huggingface.co/$Repo/resolve/main"
foreach ($f in $Files + "model.safetensors") {
    Write-Host "fetching $f"
    # into a .part file, moved into place only when complete (and, for the weights, verified):
    # a failed or damaged download never ends up under the real name
    $final = Join-Path $Dir $f
    $part = "$final.part"
    Remove-Item -Force -ErrorAction SilentlyContinue $part
    try {
        Invoke-WebRequest -Uri "$Base/$f" -Headers $Headers -OutFile $part
    } catch {
        Remove-Item -Force -ErrorAction SilentlyContinue $part
        throw "download of $Repo/$f failed: $($_.Exception.Message)"
    }
    if ($f -eq "model.safetensors") {
        Write-Host "checking model.safetensors (SHA-256 of 3.4 GB)..."
        $got = (Get-FileHash $part -Algorithm SHA256).Hash.ToLower()
        if ($got -ne $Sha256) {
            Remove-Item -Force $part
            throw "the download is damaged or not the official checkpoint (got $got); deleted it"
        }
    }
    Move-Item -Force $part $final
}
Test-Install
Write-Host "Restart the app; Object and Describe in the Masking panel now use it."
