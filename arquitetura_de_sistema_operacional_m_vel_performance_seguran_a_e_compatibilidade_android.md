# Blueprint: Sistema Operacional Móvel de Alta Performance, Segurança e Compatibilidade Android (Codinome: LangeOS)

Este documento define a arquitetura técnica, a escolha de linguagens, as estratégias de engenharia e a camada de compatibilidade Android/Google Play para construir um Sistema Operacional móvel alternativo, projetado para rodar com extrema fluidez tanto em dispositivos de entrada (512 MB - 2 GB de RAM) quanto em flagships modernos, mantendo segurança de nível militar.

---

## 1. Pilares Fundamentais

1. **Zero-Cost Abstractions & Baixo Consumo de RAM:** Eliminação de máquinas virtuais pesadas e *Garbage Collectors* (GC) tradicionais no núcleo do sistema, evitando engasgos (*stuttering*) e alto consumo de memória.
2. **Memory Safety por Padrão:** Prevenção de 70%+ das vulnerabilidades críticas (como *buffer overflows* e *use-after-free*) em tempo de compilação.
3. **Escalabilidade Adaptativa (Modularidade):** O sistema ajusta serviços, renderização, cache e o subsistema Android dinamicamente com base no hardware detectado no boot.
4. **Isolamento Estrito (Sandboxing):** Princípio do menor privilégio aplicado desde os drivers até os aplicativos nativos e Android.
5. **Compatibilidade Total com Ecossistema Android:** Capacidade de executar APKs/AABs e acessar a Google Play Store de forma nativa, segura e sob demanda.

---

## 2. Stack de Linguagens Escolhidas

Para atingir o equilíbrio perfeito entre compatibilidade de hardware, segurança e velocidade, o sistema é dividido em camadas, utilizando a melhor linguagem para cada propósito:

### Camada 1: Kernel e Drivers de Baixo Nível
* **Linguagens:** **C** (para compatibilidade com BSPs existentes) e **Rust** (para novos drivers, subsistemas e módulos de segurança).
* **Por quê?** O Kernel Linux (com módulos *Binder* e *Ashmem/Memfd* habilitados) já possui suporte massivo para SoCs (Qualcomm, MediaTek, Unisoc, Exynos) em C. A introdução de **Rust** garante segurança de memória no espaço do kernel sem perda de performance.

### Camada 2: HAL (Hardware Abstraction Layer) e Daemons do Sistema
* **Linguagem:** **Rust** (100%).
* **Por quê?** No Android, essa camada é feita em C++, fonte histórica de falhas graves de segurança. Em Rust, temos controle total de hardware, concorrência sem *data races* e binários extremamente enxutos.

### Camada 3: Compositor Gráfico e Engine de UI
* **Linguagens:** **Rust** + **C++** (integração direta com **Vulkan** e **OpenGL ES 2.0/3.0**).
* **Por quê?** Para rodar em celulares antigos (OpenGL ES 2.0) e modernos (Vulkan), o compositor precisa falar diretamente com a GPU sem camadas intermediárias pesadas, garantindo 60/120 FPS constantes e integrando janelas nativas e Android perfeitamente.

### Camada 4: Runtime de Aplicativos Nativos e SDK
* **Linguagens:** **Rust** (Core/Nativo) e **Dart / Kotlin Native (Compilados AOT)** ou **WebAssembly (Wasm)**.
* **Por quê?** Diferente do Android tradicional, os apps nativos do LangeOS são compilados **Ahead-of-Time (AOT)** para código de máquina nativo (ARM64 / ARMv7), reduzindo o uso de RAM em até 50% e iniciando instantaneamente.

### Camada 5: Subsistema de Compatibilidade Android (LangeDroid)
* **Linguagens:** **Rust** (Orquestrador de Contêineres, Bridge de IPC/Wayland e Políticas de Segurança) + **C++ / Java / Kotlin** (AOSP Runtime mínimo / ART otimizado).

---

## 3. Arquitetura do Sistema

```text
+-------------------------------------------------------------------+
|               APLICATIVOS DO USUÁRIO (Nativos & Android)          |
|   [Apps Nativos AOT/Wasm]   |   [APKs / Google Play Store / GMS]  |
+-----------------------------+-------------------------------------+
|   FRAMEWORK DE UI NATIVO    |   SUBSISTEMA ANDROID (LangeDroid)   |
|  (Widgets, APIs, Permissões)|  (Contêiner LXC + ART + OpenGApps)  |
+-----------------------------+-------------------------------------+
|            COMPOSITOR GRÁFICO UNIFICADO (Wayland + EGL)           |
|              (Vulkan / OpenGL ES Fallback Dinâmico)               |
+-------------------------------------------------------------------+
|          SERVIÇOS DE SISTEMA & HAL (100% escritos em Rust)        |
|   (Áudio, Rede, Sensores, Câmera, Telefonia, Gerenciador Energia) |
+-------------------------------------------------------------------+
|               KERNEL (Linux Hardened + Rust + Binder)             |
|        (ZRAM Otimizado, Scheduler EEVDF, SELinux/Landlock)        |
+-------------------------------------------------------------------+
|                  HARDWARE (ARM64, ARMv7, RISC-V)                  |
+-------------------------------------------------------------------+
```

---

## 4. Compatibilidade com Android e Google Play Store

Para rodar aplicativos Android e a Google Play Store sem comprometer a leveza e a segurança do sistema operacional principal, o **LangeOS** utiliza uma arquitetura de **Contêiner Isolado sob Demanda** (inspirada na evolução do Waydroid/ARC++), e não uma máquina virtual lenta.

### A. Execução Nativa via Contêineres Linux (Zero Overhead de CPU)
* O subsistema **LangeDroid** compartilha o mesmo Kernel Linux do sistema hospedeiro utilizando *Namespaces* e *Cgroups*.
* Não há emulação de CPU (em dispositivos ARM). O código nativo (NDK) e o bytecode compilado pelo **ART (Android Runtime)** rodam diretamente no processador físico com velocidade nativa (98% a 100% da performance de um Android puro).

### B. Integração com a Google Play Store e Google Mobile Services (GMS)
Para oferecer suporte completo à loja do Google, notificações push (FCM), mapas, login com conta Google e apps bancários:
1. **Imagem AOSP Enxuta (Minimal AOSP):** Uma base AOSP despojada de *bloatwares* de fabricantes, launchers redundantes e serviços duplicados (Wi-Fi, Bluetooth e RIL são gerenciados pelo host em Rust e repassados via HAL virtualizada).
2. **GMS instalado pelo usuário (não pré-instalado):** Distribuir `Google Play Services`/`GSF`/`Play Store` pré-instalados exige licença/certificação do Google. O LangeOS oferece instalação opcional pelo usuário (modelo Waydroid). Padrão de fábrica: **microG**.
3. **Play Integrity — limitação conhecida:**
   * Integridade DEVICE/STRONG depende de atestado de chave em hardware e de aparelho certificado pelo Google; expor `ro.boot.verifiedbootstate=green` **não** basta. Apps que exigem esse nível (alguns bancos/carteiras) não funcionarão. Meta realista: BASIC integrity. Ver `docs/ESTRATEGIA.md`.
4. **Alternativa Livre Opcional (microG):** Durante a configuração inicial (*Setup Wizard*), o usuário pode escolher entre ativar o **GMS Oficial (Google Play Store)** ou o **microG** (para máxima privacidade e menor consumo de RAM).

### C. Integração Visual Transparente (Multi-Window)
* O compositor gráfico **Wayland** do LangeOS se comunica diretamente com o *SurfaceFlinger* do Android.
* Cada aplicativo Android instalado aparece na tela inicial nativa do LangeOS como um ícone comum. Ao ser aberto, ele renderiza diretamente na tela do sistema sem exibir uma "janela dentro de outra janela" ou interface secundária.
* Compartilhamento unificado de área de transferência (*Clipboard*), notificações do sistema, arquivos (`/sdcard` mapeado para `/home/user/Media`) e intenções (*Intents* entre apps nativos e Android).

---

## 5. Estratégias para Rodar em Qualquer Celular (Low-End a Flagship)

Para suportar desde um aparelho com 1 GB de RAM até um topo de linha com 16 GB, o sistema implementa **Perfis de Hardware Dinâmicos**:

### A. Gerenciamento Inteligente do Subsistema Android por Perfil
* **Perfil Ultra-Light (1 GB – 2 GB de RAM):**
  * **Cold Start Sob Demanda:** O subsistema Android e o Google Play Services **não** iniciam no boot do aparelho. O sistema operacional sobe consumindo apenas ~220 MB de RAM.
  * O contêiner Android só é carregado na memória quando o usuário abre um app Android pela primeira vez e é **hibernado/congelado (`cgroup freezer`)** 30 segundos após o app ser fechado. Como o freezer só para a CPU, o LangeDroid em seguida força `memory.reclaim` no cgroup, empurrando a RAM do container para o ZRAM comprimido.
  * Uso de *Android Go Configs* (`ro.config.low_ram=true`) dentro do contêiner para reduzir o consumo do ART e do GMS.
* **Perfil Intermediário (3 GB – 4 GB de RAM):**
  * Inicialização em segundo plano com prioridade baixa (*Idle I/O*) após o boot da interface nativa.
  * Suspensão inteligente de processos em background do Google Play Services.
* **Perfil Flagship (6 GB – 16 GB+ de RAM):**
  * Inicialização paralela instantânea no boot.
  * Cache pré-carregado, suporte a múltiplas janelas Android simultâneas em 120Hz e alocação dinâmica de VRAM.

### B. Otimizações de Memória e Armazenamento no Host
* **ZRAM Dinâmico com LZ4/Zstd:** Compressão agressiva de páginas inativas na RAM, permitindo que 1 GB físico se comporte como ~1.8 GB sem depender de swap lento em memória flash (eMMC).
* **KSM (Kernel Samepage Merging):** Deduplicação de páginas de memória idênticas entre o sistema nativo e o contêiner Android.
* **Sistema de Arquivos Imutável (EROFS):** Tanto o LangeOS quanto a imagem base do sistema Android usam **EROFS** com compressão LZ4, reduzindo o espaço ocupado em disco em até 35% e acelerando a leitura sequencial.

---

## 6. Arquitetura de Segurança (Security-by-Design)

1. **Quarentena do Ecossistema Android:**
   * Mesmo que um APK malicioso explore uma vulnerabilidade dentro do contêiner Android, ele permanece preso nos *Namespaces*, *Seccomp-BPF* e políticas *SELinux/Landlock* impostas pelo kernel do LangeOS, sem acesso aos dados do sistema nativo ou ao hardware bruto.
2. **Proxy de Permissões Granulares:**
   * Quando um app Android solicita acesso à câmera, microfone ou GPS, o LangeOS intercepta a chamada na HAL virtualizada e exibe o diálogo de permissão nativo em Rust. O usuário pode fornecer **dados falsos/vazios** (ex: câmera preta ou localização aproximada) para apps invasivos sem quebrá-los.
3. **Verified Boot (AVB) e Partição Imutável:**
   * As partições `/system` (LangeOS) e `/android-system` (AOSP + Play Store) são montadas estritamente como somente leitura (`read-only`) e validadas criptograficamente no boot.
4. **Firewall Nativo via eBPF:**
   * Controle total de tráfego de rede por aplicativo (nativo ou Android), permitindo bloquear rastreadores do Google ou conexões em segundo plano diretamente no kernel.

---

## 7. Roadmap de Desenvolvimento (Passo a Passo)

> Ordem prática revisada (QEMU primeiro, fatia vertical) e status atual em `docs/ESTRATEGIA.md`.

### Fase 1: Base, Kernel e Boot (Meses 1–4)
* Configurar o toolchain de compilação cruzada para `aarch64` e `armv7`.
* Compilar um Kernel Linux LTS enxuto com patches de segurança (KSPP), suporte a Rust e módulos necessários para Android (`binder_linux`, `ashmem`/`memfd`, `psi`).
* Criar o processo `init` e o gerenciador de serviços em Rust.

### Fase 2: HAL e Subsistemas Essenciais (Meses 5–8)
* Alvo inicial: aparelhos com kernel mainline (postmarketOS). Depois, integrar o `libhybris` para reutilizar drivers proprietários de GPU/Modem/Câmera de fabricantes existentes.
* Implementar gerenciadores de rede, áudio (PipeWire) e energia em Rust.

### Fase 3: Compositor Gráfico e Interface Nativa (Meses 9–12)
* Desenvolver o compositor **Wayland** em Rust sobre **Smithay** com suporte dual (Vulkan para dispositivos modernos e OpenGL ES 2.0/3.0 para aparelhos de entrada).
* Construir a Launcher, barra de status, central de notificações e painel de configurações nativos.

### Fase 4: Subsistema LangeDroid e Google Play Store (Meses 13–16)
* Compilar uma árvore AOSP enxuta (Android 14/15/16) focada apenas em runtime e framework de apps.
* Desenvolver em Rust o daemon orquestrador do contêiner LXC e as pontes HAL (Áudio, Vídeo/Gralloc, Sensores, Rede e Notificações).
* Base do container: imagem Waydroid/LineageOS. microG por padrão; GMS opcional instalado pelo usuário.
* Implementar o sistema de hibernação automática (`cgroup freezer`) para dispositivos com 1 GB a 2 GB de RAM.

### Fase 5: SDK Nativo e Otimização Final (Meses 17–18)
* Disponibilizar o SDK oficial do LangeOS (Rust / Dart AOT) para desenvolvedores criarem apps ultra-leves.
* Ajustar perfis térmicos e de bateria para dispositivos de entrada e flagships.