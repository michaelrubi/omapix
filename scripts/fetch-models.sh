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
  "face-detect-yunet|face_detection_yunet_2023mar.onnx|https://huggingface.co/opencv/face_detection_yunet/resolve/3cc26e7f1014a5ee5d74a42acee58bafc9d0a310/face_detection_yunet_2023mar.onnx|8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4|227 KB|MIT|detect|yunet|Shiqi Yu et al. (YuNet), OpenCV Zoo|https://huggingface.co/opencv/face_detection_yunet"
  "face-landmarks-mediapipe|face_landmarks_detector.onnx|https://huggingface.co/senty-au/face_landmarks_detector-ONNX/resolve/337d58218b5b1cc597ca3c67360880b920f6ce7b/onnx/model.onnx|7d6e82dee82a1dca5fbddb282b3cc74571833a530de317fc22ae325c3358beeb|4.9 MB|Apache-2.0|landmarks|mediapipe-face-mesh-v2|Google (MediaPipe Face Landmarker), senty-au (ONNX export)|https://huggingface.co/senty-au/face_landmarks_detector-ONNX"
  "mask-selfie-multiclass|selfie_multiclass_256x256.onnx|https://huggingface.co/senty-au/selfie_multiclass_256x256-ONNX/resolve/6db8421a7150ac20558f2c24675078eb3a1a04d0/onnx/model.onnx|35ec1ecd9ee7f85073c99c00020b7f6751b69506eeacf683bc8665f6117f85b0|16 MB|Apache-2.0|mask|mediapipe-selfie-multiclass|Google (MediaPipe multiclass selfie segmentation), senty-au (ONNX export)|https://huggingface.co/senty-au/selfie_multiclass_256x256-ONNX"
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
