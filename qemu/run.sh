#!/usr/bin/env bash
# Boot AegisOS mínimo no QEMU aarch64: kernel Debian genérico + initramfs com aegis-init como /init.
# Sair do QEMU: Ctrl-A depois X.  Uso: [MEM=MB] [POWEROFF=s] ./run.sh [segundos_timeout]
set -euo pipefail
cd "$(dirname "$0")"
KERNEL_URL=https://deb.debian.org/debian/dists/stable/main/installer-arm64/current/images/netboot/debian-installer/arm64/linux
OUT=build; mkdir -p "$OUT/rootfs/etc/aegis/services" "$OUT/rootfs/bin"
# kernel próprio (../kernel/build.sh) se existir; senão Debian genérico
if [ -f ../kernel/build/Image ]; then cp ../kernel/build/Image "$OUT/Image"
elif [ ! -f "$OUT/Image" ]; then curl -fsSL -o "$OUT/Image" "$KERNEL_URL"; fi

(cd ../aegis-init && cargo build -q --release --target aarch64-unknown-linux-musl)
cp ../aegis-init/target/aarch64-unknown-linux-musl/release/aegis-init "$OUT/rootfs/init"
aarch64-linux-gnu-gcc -static -Os -o "$OUT/rootfs/bin/heartbeat" heartbeat.c
printf 'exec=/bin/heartbeat\nrestart=always\n' > "$OUT/rootfs/etc/aegis/services/00-heartbeat.svc"
printf 'exec=/bin/heartbeat\nmin_profile=flagship\n' > "$OUT/rootfs/etc/aegis/services/90-flagship-only.svc"
rm -f "$OUT/rootfs/etc/aegis/services/99-poweroff.svc"
aarch64-linux-gnu-gcc -static -Os -o "$OUT/rootfs/bin/memhog" memhog.c
printf 'exec=/bin/memhog\nrestart=never\nmemory_max=16M\n' > "$OUT/rootfs/etc/aegis/services/50-memhog.svc"
if [ -n "${POWEROFF:-}" ]; then  # POWEROFF=N: desliga após N s (teste automatizado)
  aarch64-linux-gnu-gcc -static -Os -o "$OUT/rootfs/bin/poweroff-test" poweroff.c
  printf 'exec=/bin/poweroff-test %s\nrestart=never\n' "$POWEROFF" > "$OUT/rootfs/etc/aegis/services/99-poweroff.svc"
fi

(cd "$OUT/rootfs" && find . | cpio -o -H newc --quiet --owner 0:0) | gzip > "$OUT/initramfs.cpio.gz"

${1:+timeout $1} qemu-system-aarch64 -M virt -cpu cortex-a53 -m "${MEM:-1024}" -nographic -no-reboot \
  -kernel "$OUT/Image" -initrd "$OUT/initramfs.cpio.gz" -append "console=ttyAMA0 rdinit=/init ${QUIET-quiet} panic=-1"
