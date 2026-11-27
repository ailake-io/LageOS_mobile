# Suporte a vários aparelhos — LangeOS

Status: estratégia aprovada pelo mantenedor (2026-11-27). Base: Blueprint Fase 2 + `docs/ESTRATEGIA.md`.

## 1. Princípio: núcleo comum + camada do aparelho

| Camada | Conteúdo | Muda por aparelho? |
|---|---|---|
| **Núcleo LangeOS** | `lange-init`, `langedroid`, `lange-compositor`, launcher, apps, container Android | **Não.** Uma imagem para todo ARM64. |
| **Port do aparelho** | kernel (ou kernel do fabricante), dtb, partições/fstab, tela (orientação, densidade), toque, áudio, modem | Sim, pacote pequeno |

Regras:
- O núcleo só usa interfaces padrão do Linux: DRM/KMS (vídeo), evdev (input), ALSA/PipeWire (áudio), cgroups. Perfil de hardware vem da RAM medida, não do modelo.
- Nenhum código específico de aparelho fora de `devices/`.
- QEMU (`qemu/`) é o aparelho de referência: tudo passa nele antes.

## 2. Dois caminhos de hardware

| Caminho | Como | Cobertura | Primeiro alvo |
|---|---|---|---|
| **A. Kernel mainline** (estilo postmarketOS) | Linux oficial + device tree; GPU via Mesa (freedreno, panfrost, vc4) | Poucos aparelhos, melhor qualidade | Raspberry Pi 3B (sistema base; GPU só GLES 2.0 e 1 GB de RAM: sem Android completo) |
| **B. Halium / libhybris** (estilo Ubuntu Touch) | Kernel + HALs do fabricante (partição `/vendor`, Project Treble, Android 9+); o compositor usa gralloc/hwcomposer do aparelho via libhybris; o container Android usa os HALs reais (GPU, câmera, modem) | Quase todo celular Android 9+ com bootloader desbloqueável | Samsung S21 FE (aparelho de teste; desbloqueio queima o Knox de forma permanente) |

Caminho B é o que dá escala: um núcleo, centenas de modelos, como uma GSI do Android.

## 3. Port de um aparelho

```text
devices/<fabricante>-<codinome>/
  device.conf      # nome, SoC, caminho (mainline|halium), densidade, rotação da tela
  fstab            # partições -> /system, /data, /android/...
  kernel/          # config/dtb (A) ou referência ao boot.img do fabricante (B)
  input.conf       # mapeamento do touchscreen/botões, se precisar
```

## 4. Ordem

1. QEMU (referência) — atual.
2. Pi 3B: boot, `lange-init`, compositor em HDMI, toque USB, perfil UltraLight.
3. Halium no S21 FE (ou aparelho Treble mais simples): compositor via libhybris + container com HALs reais.
4. Abrir `devices/` para ports da comunidade.
