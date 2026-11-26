# Estratégia de Desenvolvimento — LangeOS

Revisão do Blueprint e plano prático. Data: 2026-11-26.

## 1. Riscos no Blueprint (corrigir expectativa cedo)

| Item | Problema | Ajuste |
|---|---|---|
| Play Integrity "green" (§4B.3) | Expor `ro.boot.verifiedbootstate=green` **não** passa no Play Integrity. A integridade DEVICE/STRONG usa atestado de chave em hardware + lista de aparelhos certificados pelo Google. OS custom não passa; apps de banco que exigem isso não vão rodar. | Tratar como limitação conhecida. Mirar em BASIC integrity + microG. |
| GMS/OpenGApps (§4B.2) | Distribuir GMS pré-instalado exige licença Google (certificação). OpenGApps está parado. | Padrão: microG. GMS só instalado pelo usuário (como faz Waydroid). |
| libhybris (Fase 2) | Funciona, mas cada aparelho = meses de port. | Começar só com aparelhos de **kernel mainline** (postmarketOS). libhybris depois. |
| "Tudo do zero" | Compositor, container e HAL do zero = anos. | Reusar: **Smithay** (compositor Wayland em Rust), **Waydroid/LXC** (container Android), **PipeWire**. Diferencial nosso fica no que é novo. |
| 220 MB no boot (§5A) | Viável, mas só medindo. | Orçamento de RAM medido em CI a cada fase. |
| Freezer "libera a RAM" (§5A) | `cgroup freezer` só para a CPU; a RAM continua ocupada. | Freeze + `memory.reclaim` (páginas vão p/ zram). Implementado no `langedroid`. |

## 2. Princípios

1. **QEMU primeiro, sempre.** Todo componente roda e é testado no QEMU aarch64 antes de hardware real. Rápido, reproduzível, automatizável.
2. **Fatia vertical, não camadas completas.** Boot → init → container → tela, mínimo de cada, funcionando junto. Depois aprofunda.
3. **Reusar onde não somos diferentes; escrever onde somos.** Diferenciais: `lange-init` + perfis de hardware, orquestrador LangeDroid com freezer, proxy de permissões, firewall eBPF.
4. **Um aparelho real por vez.** Primeiro alvo: aparelho com mainline bom (ex: PinePhone / Pixel 3a via postmarketOS).

## 3. Ordem revisada

| # | Etapa | Entregável verificável | Status |
|---|---|---|---|
| 1 | `lange-init` (PID 1, supervisor, perfis) | testes + boot QEMU | ✅ |
| 2 | Kernel LTS próprio enxuto (binder, memfd, psi, cgroups, hardening) | boot QEMU com `kernel/build.sh` | ✅ 6.18.65, Image 6.8 MB |
| 3 | init: shutdown/reboot por sinal, cgroup v2 por serviço, log | `poweroff` limpo no QEMU (`qemu/test-boot.sh`) | ✅ sinais + cgroup v2 (`memory_max`) + log em `/data` |
| 4 | Rootfs EROFS read-only + ZRAM no boot | boot de EROFS, `zram0` ativo | ✅ `/system` EROFS lz4 ro, `/data` ext4, zram = ½ RAM |
| 5 | Orquestrador LangeDroid (Rust): namespaces + cgroup freezer, sem Android ainda | congela/descongela container de teste, mede RAM | ✅ `langedroid`; ocioso 9.2 MB → 0.3 MB |
| 6 | Android real no container (imagem Waydroid/LineageOS) | app Android abre no QEMU | 🔄 init Android 2º estágio roda no container (property service, init.rc); falta servicemanager/zygote |
| 7 | Compositor Smithay + launcher mínima (virtio-gpu) | janela nativa + janela Android na tela | |
| 8 | Primeiro aparelho físico | boot no hardware | |
| 9 | Proxy de permissões, firewall eBPF, AVB | — | |
| 10 | Integração com a distro Langeos desktop (`docs/INTEGRACAO-DESKTOP.md`) | pareamento + clipboard + notificações | planejado |

Rust dentro do kernel (§2 Camada 1) adiado: precisa `bindgen` + versão Rust casada com o kernel; nenhum driver nosso precisa disso ainda.

## 4. Processo

- Repositório: github.com/ailake-io/LangeOS_mobile (privado).
- `qemu/test-boot.sh`: teste de boot automatizado (rodar antes de fechar qualquer etapa).
- Cada etapa só fecha com entregável verificável da tabela.
