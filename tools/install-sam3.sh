#!/bin/sh
# Install the SAM 3 model for LightCraft's Object and Describe masks (see docs/ai-masks.md).
#
# The desktop app offers to download the model itself when an AI mask first needs it; this script
# is for developers (reference tests, benchmarks) and for installing from Hugging Face directly.
#
# The weights are not part of LightCraft: they are Meta's facebook/sam3 checkpoint on Hugging
# Face, under the SAM License. Access is gated: open https://huggingface.co/facebook/sam3, accept
# the license, wait for approval, then run this with a token that can read it:
#
#     HF_TOKEN=hf_... tools/install-sam3.sh
#
# (or log in once with `hf auth login`; the token it saves is used). Downloads resume when
# interrupted, and the 3.4 GB weights are checked against the official SHA-256.
#
# Options:
#     --dir DIR     where to install (default: the folder LightCraft looks in; or LIGHTCRAFT_SAM3_DIR)
#     --repo NAME   another Hugging Face repo with the same files (default: facebook/sam3)
#     --check       only verify an existing installation
set -eu

REPO="facebook/sam3"
CHECK_ONLY=0
SHA256="6d06f0a5f84e435071fe6603e61d0b4cc7b40e0d39d487cfd4d67d8cc11cc14a"
SIZE=3439938512
FILES="config.json vocab.json merges.txt tokenizer.json tokenizer_config.json special_tokens_map.json processor_config.json"

case "$(uname -s)" in
    Darwin) DEFAULT_DIR="$HOME/Library/Application Support/LightCraft/models/sam3" ;;
    *) DEFAULT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/lightcraft/models/sam3" ;;
esac
DIR="${LIGHTCRAFT_SAM3_DIR:-$DEFAULT_DIR}"

while [ $# -gt 0 ]; do
    case "$1" in
        --dir) DIR="$2"; shift 2 ;;
        --repo) REPO="$2"; shift 2 ;;
        --check) CHECK_ONLY=1; shift ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
    esac
done

sha256() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
    else sha256sum "$1" | cut -d' ' -f1; fi
}

verify() {
    for f in model.safetensors vocab.json merges.txt; do
        [ -s "$DIR/$f" ] || { echo "missing: $DIR/$f" >&2; return 1; }
    done
    echo "checking model.safetensors (SHA-256 of 3.4 GB, takes a few seconds)…"
    got=$(sha256 "$DIR/model.safetensors")
    if [ "$got" != "$SHA256" ]; then
        echo "model.safetensors does not match the official checkpoint (got $got)" >&2
        return 1
    fi
    echo "SAM 3 is installed in $DIR"
}

if [ "$CHECK_ONLY" = 1 ]; then verify; exit $?; fi

command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }

TOKEN="${HF_TOKEN:-${HUGGING_FACE_HUB_TOKEN:-}}"
if [ -z "$TOKEN" ]; then
    for t in "${HF_HOME:-$HOME/.cache/huggingface}/token" "$HOME/.huggingface/token"; do
        [ -s "$t" ] && TOKEN=$(cat "$t") && break
    done
fi
if [ -z "$TOKEN" ] && [ "$REPO" = "facebook/sam3" ]; then
    echo "No Hugging Face token found. facebook/sam3 is gated: accept the license at" >&2
    echo "https://huggingface.co/facebook/sam3, then set HF_TOKEN or run \`hf auth login\`." >&2
    exit 1
fi

mkdir -p "$DIR"
BASE="https://huggingface.co/$REPO/resolve/main"

# fetch FILE DEST [resume]: HTTP errors never reach DEST (--fail writes no error page), and a
# resume the server refuses (416) starts over instead of counting as done.
fetch() {
    set -- "$1" "$2" "${3:-}"
    if [ -n "$TOKEN" ]; then auth="Authorization: Bearer $TOKEN"; else auth="X-No-Auth: 1"; fi
    code=$(curl -fsSL ${3:+-C -} -H "$auth" -w '%{http_code}' -o "$2" "$BASE/$1") || true
    case "$code" in
        200|206) return 0 ;;
        416)
            echo "the partial download of $1 doesn't match the server's; starting over" >&2
            rm -f "$2"
            code=$(curl -fsSL -H "$auth" -w '%{http_code}' -o "$2" "$BASE/$1") || true
            [ "$code" = 200 ] && return 0
            echo "download of $REPO/$1 failed ($code)" >&2; return 1 ;;
        401|403) echo "access denied to $REPO/$1 (has your request for facebook/sam3 been approved?)" >&2; return 1 ;;
        *) echo "download of $REPO/$1 failed (${code:-no response})" >&2; return 1 ;;
    esac
}

for f in $FILES; do
    echo "fetching $f"
    # small files: whole, into place only when complete
    fetch "$f" "$DIR/$f.part" && mv -f "$DIR/$f.part" "$DIR/$f" || { rm -f "$DIR/$f.part"; exit 1; }
done

if [ -s "$DIR/model.safetensors" ] && [ "$(sha256 "$DIR/model.safetensors")" = "$SHA256" ]; then
    echo "model.safetensors already installed"
else
    echo "fetching model.safetensors (3.4 GB; resumes if interrupted)"
    part="$DIR/model.safetensors.part"
    # a wrong model.safetensors is never resumed from: it goes
    rm -f "$DIR/model.safetensors"
    if [ -f "$part" ] && [ "$(wc -c < "$part" | tr -d ' ')" -ge "$SIZE" ]; then rm -f "$part"; fi
    fetch model.safetensors "$part" resume || exit 1
    have=$(wc -c < "$part" | tr -d ' ')
    if [ "$have" -lt "$SIZE" ]; then
        echo "incomplete download ($have of $SIZE bytes); run the script again to resume" >&2
        exit 1
    fi
    echo "checking model.safetensors (SHA-256 of 3.4 GB, takes a few seconds)…"
    if [ "$have" != "$SIZE" ] || [ "$(sha256 "$part")" != "$SHA256" ]; then
        rm -f "$part"
        echo "the download is damaged or not the official checkpoint; deleted it, run the script again" >&2
        exit 1
    fi
    mv -f "$part" "$DIR/model.safetensors"
fi

verify
echo "Restart LightCraft; Object and Describe in the Masking panel now use it."
