#!/usr/bin/env python3
"""Controle mínimo do QEMU por QMP p/ testes: toque (virtio-multitouch) e screendump.
  qmp.py SOCK screendump ARQ.ppm | swipe X0 Y0 X1 Y1 | tap X Y | pinch X Y D0 D1 | quit
Coordenadas em fração da tela (0..1)."""
import json, socket, sys, time

s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); f = s.makefile('rw')
def cmd(name, **args):
    f.write(json.dumps({"execute": name, "arguments": args}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if "return" in r or "error" in r:
            if "error" in r: sys.exit(f"qmp {name}: {r['error']}")
            return r["return"]
f.readline(); cmd("qmp_capabilities")

# virtio-multitouch (QEMU): begin/update escolhem o dedo (ABS_MT_SLOT + TRACKING_ID); a posição vai em
# eventos "data" (ABS_MT_POSITION_* do slot atual); end solta (tracking-id -1).
def mt(kind, slot, x=0, y=0):
    d = lambda k, tid, axis, v: {"type": "mtt", "data": {"type": k, "slot": slot, "tracking-id": tid, "axis": axis, "value": int(v * 32767)}}
    if kind == "end": return [d("end", -1, "x", 0)]
    return [d(kind, slot + 1, "x", 0), d("data", slot + 1, "x", x), d("data", slot + 1, "y", y)]
def drag(paths):  # paths: [(x0, y0, x1, y1)] um por dedo, juntos em 10 passos (~0.5 s)
    cmd("input-send-event", events=sum((mt("begin", i, p[0], p[1]) for i, p in enumerate(paths)), []))
    for k in range(1, 11):
        time.sleep(0.05)
        cmd("input-send-event", events=sum((mt("update", i, p[0] + (p[2] - p[0]) * k / 10, p[1] + (p[3] - p[1]) * k / 10) for i, p in enumerate(paths)), []))
    cmd("input-send-event", events=sum((mt("end", i) for i in range(len(paths))), []))

op, a = sys.argv[2], [float(v) for v in sys.argv[3:]] if sys.argv[2] != "screendump" else sys.argv[3:]
if op == "screendump": cmd("screendump", filename=a[0])
elif op == "quit": cmd("quit")
elif op == "tap":  # sem passos intermediários: no QEMU 0.5 s já vira toque longo
    cmd("input-send-event", events=mt("begin", 0, *a)); cmd("input-send-event", events=mt("end", 0))
elif op == "pinch":  # pinch X Y D0 D1: dois dedos na horizontal, distância D0 -> D1 (zoom se D1 > D0)
    x, y, d0, d1 = a
    drag([(x - d0 / 2, y, x - d1 / 2, y), (x + d0 / 2, y, x + d1 / 2, y)])
else:
    drag([a])
