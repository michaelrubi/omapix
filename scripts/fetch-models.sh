#!/usr/bin/env bash
# Downloads the AI models Omapix uses (crates/omapix-ai/models.txt) into
# ~/.local/share/omapix/models, checking each file's SHA-256 (docs/AI.md,
# "Models on disk"). Omapix itself never goes online. A file already there,
# in darktable's models or in a folder listed in ~/.config/omapix/model_folders,
# is used where it is rather than downloaded again.
set -euo pipefail

manifest="$(dirname "$0")/../crates/omapix-ai/models.txt"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
dir="$data/omapix/models"
shared=("$data/darktable/models")
list="${XDG_CONFIG_HOME:-$HOME/.config}/omapix/model_folders"
if [[ -f $list ]]; then
  while IFS= read -r folder; do
    [[ -z $folder || $folder == \#* ]] || shared+=("${folder/#\~/$HOME}")
  done <"$list"
fi

# A file of `bytes` with SHA-256 `sha` in the shared folders, if there is one.
find_shared() {
  local bytes=$1 sha=$2 candidate
  for folder in "${shared[@]}"; do
    [[ -d $folder ]] || continue
    while IFS= read -r -d '' candidate; do
      if echo "$sha  $candidate" | sha256sum --check --status; then
        echo "$candidate"
        return
      fi
    done < <(find "$folder" -type f -size "${bytes}c" -print0)
  done
}

declare -A licence author source task arch
while IFS='|' read -r kind id a b c d e f g; do
  case $kind in
    model)
      licence[$id]=$c author[$id]=$d source[$id]=$e task[$id]=$f arch[$id]=$g
      echo "$a, for $b: $c"
      ;;
    file)
      file=$a bytes=$b sha=$c url=$d
      target="$dir/$id/$file"
      size="$((bytes / 1000000)) MB"
      if [[ -f $target ]] && echo "$sha  $target" | sha256sum --check --status; then
        echo "  $file: already installed"
      elif found=$(find_shared "$bytes" "$sha") && [[ -n $found ]]; then
        echo "  $file: using $found"
      elif [[ -z $url ]]; then
        echo "  $file: missing; install it from darktable's AI preferences ($size)"
      else
        echo "  $file: downloading $size"
        mkdir -p "$dir/$id"
        curl --fail --location --progress-bar --output "$target.part" "$url"
        if ! echo "$sha  $target.part" | sha256sum --check --status; then
          rm -f "$target.part"
          echo "  $file: checksum mismatch, removed the download" >&2
          exit 1
        fi
        mv "$target.part" "$target"
      fi
      # The model card, next to Omapix's own copy.
      if [[ -f $target ]]; then
        cat >"$dir/$id/config.json" <<JSON
{
    "id": "$id",
    "task": "${task[$id]}",
    "arch": "${arch[$id]}",
    "backend": "onnx",
    "version": "1.0",
    "sha256": { "$file": "$sha" },
    "model_card": {
        "author": "${author[$id]}",
        "source": "${source[$id]}",
        "license": "${licence[$id]}"
    }
}
JSON
      fi
      ;;
  esac
done < <(grep -v '^#' "$manifest")
