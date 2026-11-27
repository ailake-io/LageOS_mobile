#!/usr/bin/env bash
# Boot LangeOS + container Android no QEMU (emulado: lento). Log completo em build/boot.log.
# Passa se o init do Android chega ao 2º estágio e o servicemanager (binder) sobe. Uso: ./test-android.sh [timeout_s]
set -uo pipefail
cd "$(dirname "$0")"
# QEMU anterior vivo usando os mesmos discos = log misturado. Aborta em vez de gerar resultado falso.
# nome do processo é truncado em 15 chars no kernel
if pgrep -x qemu-system-aar >/dev/null; then echo "ERRO: outro QEMU rodando"; exit 2; fi
Q=$PWD/build/qmp.sock; rm -f "$Q" build/touch-*
QMP=$Q QUIET= ANDROID=1 ../qemu/run.sh "${1:-600}" > build/boot.log 2>&1 &
qpid=$!
# Etapa 7 (toque): quando o Android termina o boot, puxa a barra de notificações com swipe no
# virtio-tablet e compara screendumps do QEMU antes/depois.
touched=
while kill -0 $qpid 2>/dev/null; do
  if grep -aqE 'processing action \(sys.boot_completed=1\)' build/boot.log; then
    sleep 60  # Launcher só aparece ~30-40 s após boot_completed no QEMU
    python3 qmp.py "$Q" swipe 0.5 0.05 0.5 0.8 && touched=1
    sleep 15  # compositor grava screen-post-touch.ppm 4 s após soltar (com fsync) + commit do ext4
    python3 qmp.py "$Q" quit 2>/dev/null; break
  fi
  sleep 5
done
wait $qpid
grep -aq 'Failed to get "write" lock' build/boot.log && { echo "ERRO: disco em uso por outro processo"; exit 2; }
fail=0
for want in "langedroid] open -> ok state=running" "init: init second stage started" "init: starting service 'servicemanager'"; do
  grep -qF "$want" build/boot.log && echo "OK    $want" || { echo "FALTA $want"; fail=1; }
done
grep -q "Kernel panic" build/boot.log && { echo "FALHOU: kernel panic"; fail=1; }
grep -qE 'init: Reboot start|InitFatalReboot' build/boot.log && { echo "FALHOU: Android pediu reboot: $(grep -oE "Reboot start, reason: [^,]*|critical process '[^']+'" build/boot.log | head -1)"; fail=1; }
# screenshot do lange-compositor (/data do host = qemu/build/data.img)
# debugfs não aplica o journal do ext4 (QEMU morto antes do commit = blocos velhos): lê de uma
# cópia com o journal aplicado pelo e2fsck.
cp --sparse=always ../qemu/build/data.img build/data-ro.img && e2fsck -fy build/data-ro.img >/dev/null 2>&1
rm -f build/screen.ppm build/screen.png build/screen-max.ppm build/screen-max.png
debugfs -R "dump /compositor/screen.ppm build/screen.ppm" build/data-ro.img 2>/dev/null
[ -s build/screen.ppm ] && python3 ppm2png.py build/screen.ppm build/screen.png && echo "screenshot: android/build/screen.png"
debugfs -R "dump /compositor/screen-max.ppm build/screen-max.ppm" build/data-ro.img 2>/dev/null
[ -s build/screen-max.ppm ] && python3 ppm2png.py build/screen-max.ppm build/screen-max.png && echo "quadro mais aceso: android/build/screen-max.png"
# Etapa 6/7: Android visível = >= 30% da tela diferente do fundo do LangeOS (0x101418) e do preto
if [ -s build/screen.ppm ]; then
  lit=$(python3 -c "
d=open('build/screen.ppm','rb').read(); p=d.split(maxsplit=4); w,h,px=int(p[1]),int(p[2]),p[4]
n=sum(1 for i in range(0,w*h*3,3) if px[i:i+3] not in (b'\\x10\\x14\\x18', b'\\x00\\x00\\x00'))
print(n*100//(w*h))")
  [ "$lit" -ge 30 ] && echo "OK    tela Android visível (${lit}% da tela)" || { echo "FALTA tela Android visível (${lit}% da tela)"; fail=1; }
else echo "FALTA screenshot do compositor"; fail=1; fi
if [ -n "$touched" ]; then
  for f in pre post; do rm -f build/touch-$f.ppm; debugfs -R "dump /compositor/screen-$f-touch.ppm build/touch-$f.ppm" build/data-ro.img 2>/dev/null; done
  touch_diff=$(python3 -c "
a=open('build/touch-pre.ppm','rb').read(); b=open('build/touch-post.ppm','rb').read()
assert a[:2]==b[:2]==b'P6' and len(a)==len(b)
n=min(len(a),len(b))//3; print(sum(1 for i in range(0,n*3,3) if a[i:i+3]!=b[i:i+3])*100//max(n,1))" 2>/dev/null || echo 0)
  python3 ppm2png.py build/touch-post.ppm build/touch-post.png 2>/dev/null
  [ "$touch_diff" -ge 10 ] && echo "OK    toque muda a tela (${touch_diff}% mudou; android/build/touch-post.png)" || echo "AVISO toque muda a tela (${touch_diff}% mudou): toque chega ao compositor, Android ainda não reage (etapa 7, em aberto)"
else echo "AVISO teste de toque não rodou (boot não completou)"; fi
# progresso (informativo)
echo "surfaceflinger iniciado: $(grep -ac "starting service 'surfaceflinger'" build/boot.log)x" \
     "| system_server: $(grep -aqE 'Entered the Android system server|SystemServer: Entered' build/boot.log && echo sim || echo não)" \
     "| compositor clientes: $(grep -ac 'compositor\] cliente conectado' build/boot.log)" \
     "| boot completo: $(grep -aqE 'processing action \(sys.boot_completed=1\)' build/boot.log && echo sim || echo não)"
[ $fail -eq 0 ] && echo PASSOU || { echo "--- últimas linhas init/langedroid ---"; grep -E 'init:|langedroid|\[init\]' build/boot.log | tail -25; }
exit $fail
