#!/usr/bin/env bash
# Baixa imagens Android arm64 do Waydroid (LineageOS 20 / Android 13) -> build/{system,vendor}.img
# Canal OTA oficial do Waydroid; SHA-256 conferido. Uso: ./fetch.sh
set -euo pipefail
cd "$(dirname "$0")"; mkdir -p build
get() { # $1=nome  $2=url OTA json
  [ -f "build/$1.img" ] && return
  read -r url sha < <(curl -fsSL "$2" | python3 -c "import json,sys;r=max(json.load(sys.stdin)['response'],key=lambda x:x['datetime']);print(r['url'],r['id'])")
  curl -fL --retry 3 -o "build/$1.zip" "$url"
  echo "$sha  build/$1.zip" | sha256sum -c --quiet
  python3 -c "import zipfile,sys;zipfile.ZipFile(sys.argv[1]).extract(sys.argv[2]+'.img','build')" "build/$1.zip" "$1"
  rm "build/$1.zip"
}
get system https://ota.waydro.id/system/lineage/waydroid_arm64/VANILLA.json
get vendor https://ota.waydro.id/vendor/waydroid_arm64/MAINLINE.json
ls -la build/*.img
