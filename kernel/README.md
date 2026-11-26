# Kernel AegisOS

Linux LTS (padrão 6.18.65, `KVER=` p/ trocar) arm64, `tinyconfig` + `aegis.config`. Tudo built-in, sem módulos.

```
./build.sh        # baixa fonte, configura, compila -> build/Image
../qemu/test-boot.sh   # usa build/Image automaticamente
QUIET= ../qemu/run.sh 10   # boot com log completo do kernel
```

`build.sh` avisa qualquer opção do fragmento que o Kconfig descartou (dependência faltando) — build deve sair sem `AVISO`.

Inclui: binder/binderfs, memfd, PSI, cgroups+freezer, namespaces, ZRAM (LZ4), KSM, EROFS, seccomp, Landlock, BPF, hardening KSPP (stackprotector strong, fortify, KASLR, init_on_alloc, freelist hardened).

Disco: virtio-mmio + virtio-blk, ext4 (`/data`).

Ainda não: Rust no kernel (falta `bindgen`), virtio rede/gpu (etapa 7), cgroup v2 controllers além de memory/freezer.
