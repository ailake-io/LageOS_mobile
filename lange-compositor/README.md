# lange-compositor

Compositor Wayland do LangeOS (Blueprint §3 Camada 3, etapa 7). Rust + [Smithay](https://github.com/Smithay/smithay) só p/ protocolos; composição na CPU num dumb buffer DRM. Sem Mesa/EGL/pixman no host. Binário estático aarch64 ~2.6 MB.

```
lange-compositor [--socket /run/lange/wayland-0] [--size LxA] [--screenshot ARQ.ppm]
```

- Saída: `/dev/dri/card0` (primeiro conector conectado, primeiro modo). Sem DRM: headless em memória.
- Protocolos: compositor + subcompositor, xdg_shell, shm, linux-dmabuf (linear, 1 plano), viewporter, presentation, output. Cobre o hwcomposer do Waydroid (container LangeDroid).
- Janelas: tela cheia, empilhadas; subsurfaces posicionadas; viewporter = escala vizinho-mais-próximo; blend alfa pré-multiplicado.
- Shell nativo: barra de status de 32 px (relógio HH:MM) desenhada pelo compositor; janelas ficam abaixo dela.
- Toque: primeiro `/dev/input/event*` com eixos absolutos (virtio-tablet no QEMU) vira `wl_touch` no `wl_seat` "seat0", focado na superfície mais à frente sob o dedo.
- Privilégios: abre DRM, input e socket como root e então larga para uid/gid 600 (socket `600:1000 0660`).
- `--screenshot`: PPM a cada 2 s; o diretório é criado e passa a ser do compositor (usado por `android/test-android.sh`).

## Limites conhecidos (ponytail)
- Toque single-touch (ABS_X/Y + BTN_TOUCH/BTN_LEFT); multitouch `ABS_MT_*` depois. Dispositivo escolhido só na partida (sem hotplug).
- Sem teclado: o Seat do Smithay exige libxkbcommon; `build.rs` linka um stub que faz trap se chamado.
- Relógio em UTC (sem tzdata).
- **Em aberto:** o toque chega ao compositor e é entregue como `wl_touch` ao hwcomposer, mas o Android ainda não reage (swipe/tap sem efeito no QEMU). Próximo: confirmar se o hwcomposer escreve no FIFO `wl_touch_events` e o que o InputDispatcher faz com o evento.
- Loop por poll() (cliente nunca espera), quadros limitados a ~30 Hz, buffer único (sem page flip/vblank). Cópia na CPU do último buffer de cada superfície por quadro (virtio: TRANSFER_FROM_HOST antes).
- Sem `wl_shell`, popups ignorados.
