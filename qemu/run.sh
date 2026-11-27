#!/usr/bin/env bash
# Boot LangeOS no QEMU aarch64: kernel próprio + /system EROFS read-only (vda) + /data ext4 persistente (vdb).
# Sair do QEMU: Ctrl-A depois X.  Uso: [MEM=MB] [SMP=n] [POWEROFF=s] [QUIET=] [ANDROID=1] ./run.sh [segundos_timeout]
# GPU=virgl: GPU 3D do host via virglrenderer (QEMU do apt, -display egl-headless). Padrão: 2D + Mesa em software.
# ANDROID=1: + system/vendor do Waydroid (../android/fetch.sh) e container Android aberto no boot.
# Apagar build/data.img zera os dados persistentes.
set -euo pipefail
cd "$(dirname "$0")"
OUT=build
KERNEL=../kernel/build/Image
[ -f "$KERNEL" ] || { echo "falta $KERNEL: rode ../kernel/build.sh" >&2; exit 1; }

R=$OUT/rootfs; rm -rf "$R"
mkdir -p "$R"/{sbin,bin,proc,sys,dev,data,run,android/root,etc/lange/services}
for c in lange-init langedroid lange-compositor; do (cd ../$c && cargo build -q --release --target aarch64-unknown-linux-musl); done
cp ../lange-init/target/aarch64-unknown-linux-musl/release/lange-init "$R/sbin/init"
cp ../langedroid/target/aarch64-unknown-linux-musl/release/langedroid "$R/bin/langedroid"
cp ../lange-compositor/target/aarch64-unknown-linux-musl/release/lange-compositor "$R/bin/lange-compositor"
cc() { aarch64-linux-gnu-gcc -static -Os -o "$R/bin/$1" "$1.c"; }
cc heartbeat; cc memhog; cc droid-payload
printf '/dev/vdb /data ext4 rw\ntmpfs /run tmpfs rw\n' > "$R/etc/lange/fstab"
S=$R/etc/lange/services
printf 'exec=/bin/heartbeat\nrestart=always\n' > "$S/00-heartbeat.svc"
printf 'exec=/bin/memhog\nrestart=never\nmemory_max=16M\n' > "$S/50-memhog.svc"
DISKS=(); CMDLINE_EXTRA=
QEMU=qemu-system-aarch64
GPU_DEV=(-device virtio-gpu-device)  # 2D: Mesa renderiza por software (llvmpipe/kms_swrast)
if [ "${GPU:-}" = virgl ]; then
  QEMU=/usr/bin/qemu-system-aarch64  # QEMU do Ubuntu tem virglrenderer; o do Homebrew não
  GPU_DEV=(-device virtio-gpu-gl-device -display egl-headless)
fi
if [ -n "${ANDROID:-}" ]; then
  A=../android/build
  [ -f $A/system.img ] && [ -f $A/vendor.img ] || { echo "falta imagem Android: rode ../android/fetch.sh" >&2; exit 1; }
  [ -f $A/data.img ] || mkfs.ext4 -q -F $A/data.img 4G  # esparso: ocupa só o usado
  # vdc/vdd/vde (ordem de enumeração invertida: ver comentário no qemu abaixo)
  DISKS=(-drive file=$A/data.img,format=raw,if=none,id=adata -device virtio-blk-device,drive=adata
         -drive file=$A/vendor.img,format=raw,if=none,id=avendor,readonly=on -device virtio-blk-device,drive=avendor
         -drive file=$A/system.img,format=raw,if=none,id=asys,readonly=on -device virtio-blk-device,drive=asys
         "${GPU_DEV[@]}")
  printf '/dev/vdc /android/root ext4 ro\n/dev/vdd /android/root/vendor ext4 ro\n/dev/vde /android/root/data ext4 rw\n' >> "$R/etc/lange/fstab"
  # 1º estágio do init Android (mknod /dev, fstab) é trabalho do host: entra no selinux_setup
  # (carrega a sepolicy do Android, permissiva) que então chama o 2º estágio.
  # gráficos estilo Waydroid: EGL Mesa + gralloc gbm no render node
  GFX_PROPS='--prop ro.hardware.egl=mesa --prop ro.hardware.gralloc=gbm --prop gralloc.gbm.device=/dev/dri/renderD128 '
  # kernel mainline não tem ashmem (removido na 5.18): libcutils usa memfd (HWC/FMQ, zygote)
  GFX_PROPS+='--prop sys.use_memfd=true '
  # QEMU emulado (TCG) é ~10-20x mais lento: Watchdog matava o system_server (Blocked in handler).
  # Prop feita p/ emuladores (Cuttlefish); só no QEMU, aparelho real mantém os timeouts normais.
  GFX_PROPS+='--prop ro.hw_timeout_multiplier=10 '
  # virtio-gpu sem virgl não tem driver Mesa 3D: força kms_swrast/llvmpipe (env da Mesa vira prop debug.mesa.* no Android)
  [ "${GPU:-}" = virgl ] || GFX_PROPS+='--prop debug.mesa.loader.driver.override=kms_swrast --prop debug.mesa.gbm.always.software=true --prop debug.mesa.libgl.always.software=true '
  printf 'exec=/bin/langedroid daemon --rootfs /android/root --idle-freeze %s %s%s--wayland-dir /run/lange --payload /system/bin/init selinux_setup\n' "${IDLE_FREEZE:-30}" "${DEBUG_LOGCAT:+--debug-logcat }" "$GFX_PROPS" > "$S/20-langedroid.svc"
  printf 'exec=/bin/langedroid ctl open\nrestart=never\n' > "$S/30-android-open.svc"
  MEM=${MEM:-3072}
  # compositor sobe antes do container (10 < 20); screenshot em /data p/ o teste extrair
  printf 'exec=/bin/lange-compositor --socket /run/lange/wayland-0 --screenshot /data/screen.ppm\n' > "$S/10-compositor.svc"
  CMDLINE_EXTRA="printk.devkmsg=on androidboot.selinux=permissive"  # sem ratelimit: logs do init Android completos
  rm -f "$S"/{00-heartbeat,50-memhog,90-flagship-only}.svc  # sem ruído no log do Android
else
  printf 'exec=/bin/langedroid daemon --idle-freeze %s\n' "${IDLE_FREEZE:-30}" > "$S/20-langedroid.svc"
fi
printf 'exec=/bin/heartbeat\nmin_profile=flagship\n' > "$S/90-flagship-only.svc"
if [ -n "${POWEROFF:-}" ]; then  # POWEROFF=N: desliga após N s (teste automatizado)
  cc poweroff
  printf 'exec=/bin/poweroff %s\nrestart=never\n' "$POWEROFF" > "$S/99-poweroff.svc"
  printf 'exec=/bin/langedroid selftest\nrestart=never\n' > "$S/30-droid-selftest.svc"
fi

mkfs.erofs --quiet -zlz4 --all-root "$OUT/system.erofs" "$R" >/dev/null
[ -f "$OUT/data.img" ] || mkfs.ext4 -q -F "$OUT/data.img" 64M

# virtio-mmio moderno (1.0): virtio-gpu recusa o transporte legado em silêncio.
# virtio-mmio no virt enumera ao contrário: último -device vira vda. Por isso data antes de system.
${1:+timeout $1} $QEMU -M virt -cpu cortex-a53 -smp "${SMP:-4}" -m "${MEM:-1024}" -nographic -no-reboot \
  -global virtio-mmio.force-legacy=false \
  -kernel "$KERNEL" "${DISKS[@]}" \
  -drive file="$OUT/data.img",format=raw,if=none,id=data -device virtio-blk-device,drive=data \
  -drive file="$OUT/system.erofs",format=raw,if=none,id=sys,readonly=on -device virtio-blk-device,drive=sys \
  -append "console=ttyAMA0 root=/dev/vda rootfstype=erofs ro init=/sbin/init ${QUIET-quiet} panic=-1 $CMDLINE_EXTRA"
