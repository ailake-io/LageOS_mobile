# lange-compositor

Compositor Wayland do LangeOS (Blueprint §3 Camada 3, etapa 7). Rust + [Smithay](https://github.com/Smithay/smithay) só p/ protocolos; composição na CPU num dumb buffer DRM. Sem Mesa/EGL/pixman no host. Binário estático aarch64 ~2.6 MB.

```
lange-compositor [--socket /run/lange/wayland-0] [--size LxA] [--screenshot ARQ.ppm]
```

- Saída: `/dev/dri/card0` (primeiro conector conectado, primeiro modo). Sem DRM: headless em memória.
- Protocolos: compositor + subcompositor, xdg_shell, shm, linux-dmabuf (linear, 1 plano), viewporter, presentation, output. Cobre o hwcomposer do Waydroid (container LangeDroid).
- Janelas: tela cheia, empilhadas; subsurfaces posicionadas; viewporter = escala vizinho-mais-próximo; blend alfa pré-multiplicado.
- `--screenshot`: PPM a cada 2 s (usado por `android/test-android.sh`).

## Limites conhecidos (ponytail)
- Compositor roda como root (precisa ser DRM master). Separar privilégio: só o fd DRM, resto sem root.
- Sem input (`wl_seat` global não criado). O Seat do Smithay exige libxkbcommon; `build.rs` linka um stub que faz trap se chamado.
- Loop por poll() (cliente nunca espera), quadros limitados a ~30 Hz, buffer único (sem page flip/vblank). Cópia na CPU do último buffer de cada superfície por quadro (virtio: TRANSFER_FROM_HOST antes).
- Sem `wl_shell`, popups ignorados.
