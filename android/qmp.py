#!/usr/bin/env python3
"""Controle mínimo do QEMU por QMP p/ testes: toque (virtio-tablet) e screendump.
  qmp.py SOCK screendump ARQ.ppm | swipe X0 Y0 X1 Y1 | tap X Y | quit
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

def abs_ev(x, y): return [{"type": "abs", "data": {"axis": "x", "value": int(x * 32767)}},
                          {"type": "abs", "data": {"axis": "y", "value": int(y * 32767)}}]
def btn(down): return [{"type": "btn", "data": {"down": down, "button": "left"}}]

op, a = sys.argv[2], [float(v) for v in sys.argv[3:]] if sys.argv[2] != "screendump" else sys.argv[3:]
if op == "screendump": cmd("screendump", filename=a[0])
elif op == "quit": cmd("quit")
elif op == "tap":  # sem passos intermediários: no QEMU 0.5 s já vira toque longo
    cmd("input-send-event", events=abs_ev(*a) + btn(True)); cmd("input-send-event", events=btn(False))
else:
    x0, y0, x1, y1 = a
    cmd("input-send-event", events=abs_ev(x0, y0)); cmd("input-send-event", events=btn(True))
    for i in range(1, 11):  # 10 passos em ~0.5 s
        time.sleep(0.05); cmd("input-send-event", events=abs_ev(x0 + (x1 - x0) * i / 10, y0 + (y1 - y0) * i / 10))
    cmd("input-send-event", events=btn(False))
