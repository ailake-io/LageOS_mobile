# aegisdroid

Orquestrador do container Android (Blueprint §4A, §5A). Rust, zero dependências. Roda como serviço do `aegis-init`.

```
aegisdroid daemon [--idle-freeze SEG] [--payload CMD...]   # padrão: 30s, /bin/droid-payload
aegisdroid ctl open|close|freeze|thaw|status|stop
aegisdroid selftest                                         # teste ponta a ponta (usado por qemu/test-boot.sh)
```

## Container
- Namespaces novos: pid (payload é PID 1), mount (`/proc` próprio), uts (hostname `aegisdroid`), ipc, net.
- cgroup v2 `/sys/fs/cgroup/android` (irmão dos serviços, p/ ter controlador memory).
- Controle por `/run/aegisdroid.sock` (modo 0600, só root).

## Ciclo (perfil Ultra-Light)
| Comando | Efeito |
|---|---|
| `open` | parado → cold start; congelado → descongela; cancela timer |
| `close` | arma timer: após `--idle-freeze` s, congela **e** `memory.reclaim` |
| `stop` | `cgroup.kill` (container inteiro) |

Freeze sozinho só para a CPU; a RAM continua ocupada. Por isso o auto-freeze também faz reclaim, que empurra as páginas p/ zram. Medido no QEMU: 9196 KB → 328 KB.

## Ainda não
`pivot_root` p/ imagem Android (etapa 6; hoje usa a raiz do host, read-only), rede no container, política por perfil (Mid/Flagship), epoll no lugar de polling 100ms.
