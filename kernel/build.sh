#!/usr/bin/env bash
# Compila kernel LTS enxuto do AegisOS p/ arm64 -> build/Image. Uso: ./build.sh
set -euo pipefail
cd "$(dirname "$0")"
VER=${KVER:-6.18.65}
SRC=build/linux-$VER
mkdir -p build
if [ ! -d "$SRC" ]; then
  curl -fsSL "https://cdn.kernel.org/pub/linux/kernel/v${VER%%.*}.x/linux-$VER.tar.xz" | tar -xJ -C build
fi
export ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
make -C "$SRC" -s tinyconfig
"$SRC/scripts/kconfig/merge_config.sh" -m -O "$SRC" "$SRC/.config" aegis.config >/dev/null
make -C "$SRC" -s olddefconfig
# avisa opções do fragmento que o Kconfig descartou (dependência faltando)
grep -E '^(CONFIG_|# CONFIG_.* is not set)' aegis.config | while read -r l; do grep -qxF "$l" "$SRC/.config" || echo "AVISO: não aplicado: $l"; done
make -C "$SRC" -s -j"$(nproc)" Image
cp "$SRC/arch/arm64/boot/Image" build/Image
ls -la build/Image
