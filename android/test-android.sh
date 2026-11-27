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
  # launcher nativa recebeu a lista de apps do Android (langedroid `apps`)
  icon=$(grep -aoE 'compositor\] icone Galeria [0-9]+ [0-9]+' build/boot.log | head -1)
  if [ -n "$icon" ]; then
    sleep 20
    read -r _ _ _ x y <<<"$icon"
    # sistema começa bloqueado: deslizar p/ cima desbloqueia
    python3 qmp.py "$Q" swipe 0.5 0.8 0.5 0.3 && sleep 3
    # coordenadas em fração da tela (1280x800 no QEMU)
    python3 qmp.py "$Q" tap "$(python3 -c "print($x/1280)")" "$(python3 -c "print($y/800)")" && touched=1
    sleep 30  # compositor grava screen-post-touch.ppm 20 s após soltar (com fsync) + commit do ext4
    # Voltar nativo (◁ da barra de navegação, x = meio - 140) na 1ª tela da Galeria -> tela inicial nativa
    python3 qmp.py "$Q" tap "$(python3 -c "print((640-140)/1280)")" "$(python3 -c "print(776/800)")"
    for _ in $(seq 24); do grep -aq 'key back -> ok home' build/boot.log && break; sleep 5; done
    # painel rápido: puxar a barra de status p/ baixo -> notificações do Android
    python3 qmp.py "$Q" swipe 0.5 0.01 0.5 0.5
    for _ in $(seq 60); do grep -aq 'langedroid\] notifs ->' build/boot.log && break; sleep 5; done  # dumpsys é lento no QEMU
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
  grep -aq 'langedroid\] notifs -> ok' build/boot.log && echo "OK    painel rápido leu as notificações do Android" || { echo "FALTA painel rápido ler notificações"; fail=1; }
  grep -aq 'key back -> ok home' build/boot.log && echo "OK    voltar no app volta à tela inicial nativa" || { echo "FALTA voltar no app voltar à tela inicial nativa"; fail=1; }
  grep -aq 'launch com.android.gallery3d/[^ ]* -> ok' build/boot.log && echo "OK    launcher nativa abriu a Galeria (langedroid launch)" || { echo "FALTA launcher nativa abrir a Galeria"; fail=1; }
  [ "$touch_diff" -ge 2 ] && echo "OK    toque muda a tela (${touch_diff}% mudou; android/build/touch-post.png)" || { echo "FALTA toque muda a tela (${touch_diff}% mudou)"; fail=1; }
else echo "FALTA teste de toque (boot não completou)"; fail=1; fi
# progresso (informativo)
echo "surfaceflinger iniciado: $(grep -ac "starting service 'surfaceflinger'" build/boot.log)x" \
     "| system_server: $(grep -aqE 'Entered the Android system server|SystemServer: Entered' build/boot.log && echo sim || echo não)" \
     "| compositor clientes: $(grep -ac 'compositor\] cliente conectado' build/boot.log)" \
     "| boot completo: $(grep -aqE 'processing action \(sys.boot_completed=1\)' build/boot.log && echo sim || echo não)"
[ $fail -eq 0 ] && echo PASSOU || { echo "--- últimas linhas init/langedroid ---"; grep -E 'init:|langedroid|\[init\]' build/boot.log | tail -25; }
exit $fail
