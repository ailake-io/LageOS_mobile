#!/usr/bin/env bash
# Baixa imagens Android arm64 do Waydroid (LineageOS 20 / Android 13) -> build/{system,vendor}.img
# Canal OTA oficial do Waydroid; SHA-256 conferido. Uso: [ARCH=x86_64] ./fetch.sh
# ARCH=x86_64: imagens x86_64 (teste rápido com KVM) -> build/x86_64/{system,vendor}.img
set -euo pipefail
cd "$(dirname "$0")"
if [ "${ARCH:-arm64}" = x86_64 ]; then W=x86_64; B=build/x86_64; else W=arm64; B=build; fi
mkdir -p $B
get() { # $1=nome  $2=url OTA json
  [ -f "$B/$1.img" ] && return
  read -r url sha < <(curl -fsSL "$2" | python3 -c "import json,sys;r=max(json.load(sys.stdin)['response'],key=lambda x:x['datetime']);print(r['url'],r['id'])")
  curl -fL --retry 3 -o "$B/$1.zip" "$url"
  echo "$sha  $B/$1.zip" | sha256sum -c --quiet
  python3 -c "import zipfile,sys;zipfile.ZipFile(sys.argv[1]).extract(sys.argv[2]+'.img',sys.argv[3])" "$B/$1.zip" "$1" "$B"
  rm "$B/$1.zip"
}
get system https://ota.waydro.id/system/lineage/waydroid_$W/VANILLA.json
get vendor https://ota.waydro.id/vendor/waydroid_$W/MAINLINE.json
ls -la $B/*.img
