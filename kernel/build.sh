#!/usr/bin/env bash
# Compila kernel LTS enxuto do LangeOS p/ arm64 -> build/Image. Uso: [ARCH=x86_64] ./build.sh
# ARCH=x86_64: kernel p/ teste rápido no PC com KVM -> build/bzImage (fonte em build/x86_64/, + lange-x86_64.config)
set -euo pipefail
cd "$(dirname "$0")"
VER=${KVER:-6.18.65}
if [ "${ARCH:-arm64}" = x86_64 ]; then
  D=build/x86_64; FRAG=(lange.config lange-x86_64.config); IMG=arch/x86/boot/bzImage; DST=build/bzImage
  export ARCH=x86_64; unset CROSS_COMPILE
else
  D=build; FRAG=(lange.config); IMG=arch/arm64/boot/Image; DST=build/Image
  export ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
fi
SRC=$D/linux-$VER
mkdir -p "$D"
if [ ! -d "$SRC" ]; then
  if [ -d build/linux-$VER ] && [ "$D" != build ]; then cp -r build/linux-$VER "$SRC" && make -C "$SRC" -s mrproper
  else curl -fsSL "https://cdn.kernel.org/pub/linux/kernel/v${VER%%.*}.x/linux-$VER.tar.xz" | tar -xJ -C "$D"; fi
fi
make -C "$SRC" -s tinyconfig
"$SRC/scripts/kconfig/merge_config.sh" -m -O "$SRC" "$SRC/.config" "${FRAG[@]}" >/dev/null
make -C "$SRC" -s olddefconfig
# avisa opções do fragmento que o Kconfig descartou (dependência faltando)
cat "${FRAG[@]}" | grep -E '^(CONFIG_|# CONFIG_.* is not set)' | while read -r l; do grep -qxF "$l" "$SRC/.config" || echo "AVISO: não aplicado: $l"; done
make -C "$SRC" -s -j"$(nproc)" "$(basename $IMG)"
cp "$SRC/$IMG" "$DST"
ls -la "$DST"
