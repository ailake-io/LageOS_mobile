#!/usr/bin/env bash
# Boot AegisOS no QEMU aarch64: kernel próprio + /system EROFS read-only (vda) + /data ext4 persistente (vdb).
# Sair do QEMU: Ctrl-A depois X.  Uso: [MEM=MB] [POWEROFF=s] [QUIET=] ./run.sh [segundos_timeout]
# Apagar build/data.img zera os dados persistentes.
set -euo pipefail
cd "$(dirname "$0")"
OUT=build
KERNEL=../kernel/build/Image
[ -f "$KERNEL" ] || { echo "falta $KERNEL: rode ../kernel/build.sh" >&2; exit 1; }

R=$OUT/rootfs; rm -rf "$R"
mkdir -p "$R"/{sbin,bin,proc,sys,dev,data,etc/aegis/services}
(cd ../aegis-init && cargo build -q --release --target aarch64-unknown-linux-musl)
cp ../aegis-init/target/aarch64-unknown-linux-musl/release/aegis-init "$R/sbin/init"
cc() { aarch64-linux-gnu-gcc -static -Os -o "$R/bin/$1" "$1.c"; }
cc heartbeat; cc memhog
printf '/dev/vdb /data ext4 rw\n' > "$R/etc/aegis/fstab"
S=$R/etc/aegis/services
printf 'exec=/bin/heartbeat\nrestart=always\n' > "$S/00-heartbeat.svc"
printf 'exec=/bin/memhog\nrestart=never\nmemory_max=16M\n' > "$S/50-memhog.svc"
printf 'exec=/bin/heartbeat\nmin_profile=flagship\n' > "$S/90-flagship-only.svc"
if [ -n "${POWEROFF:-}" ]; then  # POWEROFF=N: desliga após N s (teste automatizado)
  cc poweroff
  printf 'exec=/bin/poweroff %s\nrestart=never\n' "$POWEROFF" > "$S/99-poweroff.svc"
fi

mkfs.erofs --quiet -zlz4 --all-root "$OUT/system.erofs" "$R" >/dev/null
[ -f "$OUT/data.img" ] || mkfs.ext4 -q -F "$OUT/data.img" 64M

# virtio-mmio no virt enumera ao contrário: último -device vira vda. Por isso data antes de system.
${1:+timeout $1} qemu-system-aarch64 -M virt -cpu cortex-a53 -m "${MEM:-1024}" -nographic -no-reboot \
  -kernel "$KERNEL" \
  -drive file="$OUT/data.img",format=raw,if=none,id=data -device virtio-blk-device,drive=data \
  -drive file="$OUT/system.erofs",format=raw,if=none,id=sys,readonly=on -device virtio-blk-device,drive=sys \
  -append "console=ttyAMA0 root=/dev/vda rootfstype=erofs ro init=/sbin/init ${QUIET-quiet} panic=-1"
