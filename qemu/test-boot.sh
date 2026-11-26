#!/usr/bin/env bash
# Teste de boot automatizado: sobe, checa supervisão e poweroff limpo. Exit 0 = passou.
set -uo pipefail
cd "$(dirname "$0")"
log=$(POWEROFF=6 ./run.sh 60 2>&1); rc=$?
fail=0
for want in "perfil de hardware: UltraLight" "start 00-heartbeat" "start 00-heartbeat pid=" "desligando (poweroff)"; do
  grep -qF "$want" <<<"$log" || { echo "FALHOU: falta '$want'"; fail=1; }
done
[ $rc -eq 124 ] && { echo "FALHOU: timeout (QEMU não desligou)"; fail=1; }
grep -q "Kernel panic" <<<"$log" && { echo "FALHOU: kernel panic"; fail=1; }
[ $fail -eq 0 ] && echo "PASSOU" || { echo "--- log ---"; tail -30 <<<"$log"; }
exit $fail
