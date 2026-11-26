#!/usr/bin/env bash
# Teste de boot automatizado: sobe, checa supervisão, cgroup, disco, zram, log persistente e poweroff. Exit 0 = passou.
set -uo pipefail
cd "$(dirname "$0")"
boots() { [ -f build/data.img ] || { echo 0; return; }; debugfs -R "cat /log/init.log" build/data.img 2>/dev/null | grep -c "perfil de hardware"; }
before=$(boots)
log=$(POWEROFF=6 ./run.sh 60 2>&1); rc=$?
fail=0
for want in "perfil de hardware: UltraLight" "start 00-heartbeat pid=" "desligando (poweroff)" \
            "50-memhog saiu (status=9)" "montado /dev/vdb em /data (ext4)" "zram swap ativo: "; do
  grep -qF "$want" <<<"$log" || { echo "FALHOU: falta '$want'"; fail=1; }
done
[ $rc -eq 124 ] && { echo "FALHOU: timeout (QEMU não desligou)"; fail=1; }
grep -q "alocou 64 MB sem ser morto" <<<"$log" && { echo "FALHOU: memory_max ignorado"; fail=1; }
grep -q "Kernel panic" <<<"$log" && { echo "FALHOU: kernel panic"; fail=1; }
after=$(boots)
[ "$after" -eq $((before + 1)) ] || { echo "FALHOU: log persistente ($before -> $after boots em /data/log/init.log)"; fail=1; }
[ $fail -eq 0 ] && echo "PASSOU ($after boots no log persistente)" || { echo "--- log ---"; tail -30 <<<"$log"; }
exit $fail
