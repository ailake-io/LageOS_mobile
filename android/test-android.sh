#!/usr/bin/env bash
# Boot LangeOS + container Android no QEMU (emulado: lento). Log completo em build/boot.log.
# Passa se o init do Android chega ao 2º estágio e o servicemanager (binder) sobe. Uso: ./test-android.sh [timeout_s]
set -uo pipefail
cd "$(dirname "$0")"
# QEMU anterior vivo usando os mesmos discos = log misturado. Aborta em vez de gerar resultado falso.
# nome do processo é truncado em 15 chars no kernel
if pgrep -x qemu-system-aar >/dev/null; then echo "ERRO: outro QEMU rodando"; exit 2; fi
QUIET= ANDROID=1 ../qemu/run.sh "${1:-600}" > build/boot.log 2>&1
grep -aq 'Failed to get "write" lock' build/boot.log && { echo "ERRO: disco em uso por outro processo"; exit 2; }
fail=0
for want in "langedroid] open -> ok state=running" "init: init second stage started" "init: starting service 'servicemanager'"; do
  grep -qF "$want" build/boot.log && echo "OK    $want" || { echo "FALTA $want"; fail=1; }
done
grep -q "Kernel panic" build/boot.log && { echo "FALHOU: kernel panic"; fail=1; }
grep -qE 'init: Reboot start|InitFatalReboot' build/boot.log && { echo "FALHOU: Android pediu reboot: $(grep -oE "Reboot start, reason: [^,]*|critical process '[^']+'" build/boot.log | head -1)"; fail=1; }
# progresso (informativo)
echo "surfaceflinger iniciado: $(grep -ac "starting service 'surfaceflinger'" build/boot.log)x" \
     "| system_server: $(grep -aqE 'Entered the Android system server' build/boot.log && echo sim || echo não)" \
     "| boot completo: $(grep -aqE 'sys.boot_completed|Boot is finished|BOOT_COMPLETED' build/boot.log && echo sim || echo não)"
[ $fail -eq 0 ] && echo PASSOU || { echo "--- últimas linhas init/langedroid ---"; grep -E 'init:|langedroid|\[init\]' build/boot.log | tail -25; }
exit $fail
