#!/usr/bin/env bash
# Boot LangeOS + container Android no QEMU (emulado: lento). Log completo em build/boot.log.
# Passa se o init do Android chega ao 2º estágio e o servicemanager (binder) sobe. Uso: ./test-android.sh [timeout_s]
set -uo pipefail
cd "$(dirname "$0")"
QUIET= ANDROID=1 ../qemu/run.sh "${1:-600}" > build/boot.log 2>&1
fail=0
for want in "langedroid] open -> ok state=running" "init: init second stage started" "init: starting service 'servicemanager'"; do
  grep -qF "$want" build/boot.log && echo "OK    $want" || { echo "FALTA $want"; fail=1; }
done
grep -q "Kernel panic" build/boot.log && { echo "FALHOU: kernel panic"; fail=1; }
grep -qE 'init: Reboot start|InitFatalReboot' build/boot.log && { echo "FALHOU: Android pediu reboot: $(grep -oE "Reboot start, reason: [^,]*|critical process '[^']+'" build/boot.log | head -1)"; fail=1; }
[ $fail -eq 0 ] && echo PASSOU || { echo "--- últimas linhas init/langedroid ---"; grep -E 'init:|langedroid|\[init\]' build/boot.log | tail -25; }
exit $fail
