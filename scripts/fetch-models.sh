#!/usr/bin/env bash
# Downloads the AI models Omapix uses into ~/.local/share/omapix/models,
# checking each file's SHA-256 (docs/AI.md, "Models on disk"). Omapix itself
# never goes online. Models already there are left alone.
set -euo pipefail

dir="${XDG_DATA_HOME:-$HOME/.local/share}/omapix/models"

# id, file, URL, SHA-256, size, licence, then darktable's config.json
# fields: task, arch, author, source.
models=(
  "inpaint-lama|lama_fp32.onnx|https://huggingface.co/Carve/LaMa-ONNX/resolve/c3c0c9e468934d62e79c329e35d82dd09ff8c444/lama_fp32.onnx|1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6|208 MB|Apache-2.0 (trained on Places2)|inpaint|lama|Samsung AI Center (LaMa), Carve (ONNX export)|https://huggingface.co/Carve/LaMa-ONNX"
)

for entry in "${models[@]}"; do
  IFS='|' read -r id file url sha size licence task arch author source <<<"$entry"
  target="$dir/$id/$file"
  if [[ -f $target ]] && echo "$sha  $target" | sha256sum --check --status; then
    echo "$id: already installed"
  else
    echo "$id: downloading $file ($size, $licence)"
    mkdir -p "$dir/$id"
    curl --fail --location --progress-bar --output "$target.part" "$url"
    if ! echo "$sha  $target.part" | sha256sum --check --status; then
      rm -f "$target.part"
      echo "$id: checksum mismatch, removed the download" >&2
      exit 1
    fi
    mv "$target.part" "$target"
  fi
  cat >"$dir/$id/config.json" <<JSON
{
    "id": "$id",
    "task": "$task",
    "arch": "$arch",
    "backend": "onnx",
    "version": "1.0",
    "sha256": { "$file": "$sha" },
    "model_card": {
        "author": "$author",
        "source": "$source",
        "license": "$licence"
    }
}
JSON
done
