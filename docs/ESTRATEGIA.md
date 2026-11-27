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
| Input do host no container | O ueventd do Android criava `/dev/input/event*` a partir do `/sys` do host: o container lia o touchscreen direto (toque duplicado). | Resolvido: `/dev/input` do container é tmpfs read-only só com os FIFOs do Waydroid. |
| SELinux do container (§6.1) | A política SELinux é global no kernel: carregar a sepolicy do Android (necessária p/ `servicemanager`/`apexd`) vale também p/ o host. Hoje: permissivo. | Antes do modo enforcing: política unificada host + container (domínios do LangeOS + sepolicy Android), carregada pelo `lange-init`. |
| 32-bit (§5) | Núcleos ARMv9 não executam AArch32; zygote32 gasta RAM residente. | Android **64-bit only** por padrão (`langedroid`); `--allow-32bit` + `CONFIG_COMPAT` p/ apps legados. |
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
| 6 | Android real no container (imagem Waydroid/LineageOS 20) | app Android abre no QEMU | ✅ boot completo em ~2,5 min; Launcher na tela (QEMU `GPU=virgl`) |
| 7 | Compositor Smithay + launcher mínima (virtio-gpu) | janela nativa + janela Android na tela | 🔄 `lange-compositor` mostra o Android abaixo da barra de status nativa; sem root; **toque funciona**; **launcher nativa** (grade de apps do Android, recentes, barra de navegação ◁ ○ □, Voltar no app raiz volta à tela inicial). Falta: ícones reais, gestos/páginas, painel rápido, teclado, multitouch, tela de bloqueio, desempenho |
| 7b | Modo janela no emulador (`WINDOW=1`: QEMU `-display gtk,gl=on`, mouse = toque) | usar a interface no PC antes do celular | planejado (após interface + apps) |
| 8 | Primeiro aparelho físico | boot no hardware | |
| 9 | Proxy de permissões, firewall eBPF, AVB | — | |
| 10 | Integração com a distro Langeos desktop (`docs/INTEGRACAO-DESKTOP.md`) | pareamento + clipboard + notificações | planejado |

Rust dentro do kernel (§2 Camada 1) adiado: precisa `bindgen` + versão Rust casada com o kernel; nenhum driver nosso precisa disso ainda.

## 4. Processo

- Repositório: github.com/ailake-io/LangeOS_mobile (privado).
- `qemu/test-boot.sh`: teste de boot automatizado (rodar antes de fechar qualquer etapa).
- Cada etapa só fecha com entregável verificável da tabela.
