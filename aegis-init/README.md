# aegis-init

PID 1 + supervisor de serviços do AegisOS (Blueprint, Fase 1). Rust, zero dependências, binário estático.

## Faz
- Como PID 1: monta `/proc`, `/sys`, `/dev`; reaping de zumbis; nunca sai.
- Sinais: `SIGTERM`/`SIGUSR1` = poweroff, `SIGINT` (Ctrl-Alt-Del) = reboot. TERM em todos, 3s, KILL, sync. Fora do PID 1 mata só filhos.
- Detecta perfil de hardware via `/proc/meminfo`: `UltraLight` (≤2 GB), `Mid` (≤4.5 GB), `Flagship`.
- Lê `<dir>/*.svc` (padrão `/etc/aegis/services`), sobe em ordem alfabética, reinicia (`restart=always`, backoff 1s).
- `min_profile` pula serviços pesados em aparelhos fracos (ex: AegisDroid em `mid`).

## Formato `.svc`
```
exec=/usr/bin/aegisdroid --lazy
restart=always          # always|never
min_profile=mid         # ultralight|mid|flagship
```
`exec` divide por espaço (sem aspas/escape).

## Uso
```
cargo test
cargo run -- examples/services            # host
cargo build --release --target aarch64-unknown-linux-musl   # celular (rust-lld)
```

## Ainda não
Dependências entre serviços, cgroups por serviço, log persistente.

## Boot no QEMU
`../qemu/run.sh [timeout]` (`MEM=8192` p/ perfil Flagship). Sair: Ctrl-A, X.
