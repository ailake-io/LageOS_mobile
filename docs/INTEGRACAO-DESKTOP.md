# Integração LangeOS mobile ↔ Langeos (distro Linux desktop)

Status: **planejado (futuro)**. Nada implementado ainda. Registrado em 2026-11-26 a pedido do mantenedor.

O LangeOS mobile e a distro Langeos são da mesma família. Objetivo: celular e PC funcionando como um sistema só, no estilo Continuity (Apple) / Phone Link (Windows) / KDE Connect, mas nativo dos dois lados.

## 1. Recursos desejados

| Recurso | Descrição |
|---|---|
| Pareamento | Pareio seguro uma vez (QR code ou código curto), chaves trocadas e guardadas nos dois lados. |
| Notificações | Notificações do celular (nativas e Android) aparecem no desktop; responder/dispensar de lá. |
| Área de transferência | Copiar num, colar no outro. |
| Arquivos | Enviar arquivos nos dois sentidos; `/home/user/Media` do celular navegável no desktop. |
| Apps Android no desktop | Espelhar/abrir apps do container LangeDroid em janela no desktop Langeos. |
| SMS / chamadas | Ler e enviar SMS, atender chamada pelo PC. |
| Internet | Hotspot com um clique a partir do desktop. |
| Controle | Desktop como teclado/mouse do celular e vice-versa. |
| Backup | `/data` do celular com backup criptografado para o desktop. |

## 2. Princípios

1. **Protocolo aberto e único**, implementado nos dois lados (crate Rust compartilhado). Nada de nuvem obrigatória: funciona em LAN/Bluetooth sem conta.
2. **Criptografia ponta a ponta** com as chaves do pareamento; nada trafega em texto claro.
3. **Permissões granulares** (mesma filosofia do proxy de permissões, Blueprint §6.2): o usuário escolhe por recurso o que o desktop pode ver.
4. **Barato no celular:** o daemon de integração é um serviço do `lange-init` com `min_profile` e `memory_max` próprios e acorda sob demanda (não pode manter o container Android acordado à toa).
5. **Compatível com KDE Connect** se der pouco trabalho: abre a porta p/ outras distros e para o Android puro.

## 3. Arquitetura proposta

```text
 LangeOS mobile                                  Langeos desktop
+---------------------------+                   +---------------------------+
| lange-link (serviço Rust) |<== TLS / Noise ==>| lange-link (serviço Rust) |
|  - notificações (nativo + |   LAN (mDNS) ou   |  - integração com o shell |
|    ponte LangeDroid)      |   Bluetooth       |    do desktop             |
|  - clipboard, arquivos    |                   |  - clipboard, arquivos    |
+---------------------------+                   +---------------------------+
        crate compartilhado `lange-link-proto` (mensagens, pareamento, cripto)
```

- **Descoberta:** mDNS na LAN; Bluetooth LE quando não houver rede comum.
- **Transporte:** TLS 1.3 com certificado fixado no pareamento (ou Noise). Um canal multiplexado por recurso.
- **Apps Android no desktop:** o compositor do mobile (etapa 7) exporta a superfície do app via stream de vídeo (Wayland → encoder de hardware), e o desktop abre em janela.

## 4. Dependências no roadmap mobile

| Precisa de | Etapa (`docs/ESTRATEGIA.md`) |
|---|---|
| Rede no host e no container | 6–7 |
| Ponte de notificações do LangeDroid | 6 |
| Compositor (p/ espelhar apps) | 7 |
| Hardware real (Bluetooth, telefonia) | 8 |
| Proxy de permissões | 9 |

Sugestão: começar pelo **pareamento + clipboard + notificações** logo depois da etapa 7.

## 5. Em aberto (decidir com o mantenedor)

- Qual desktop/compositor a distro Langeos usa (GNOME, KDE, outro)? Define a integração com o shell.
- Repositório do protocolo: crate separado, neste repo ou no repo da distro?
- Compatibilidade com KDE Connect: sim/não?
- Quer sincronização via nuvem própria (fora da LAN) ou só local?
