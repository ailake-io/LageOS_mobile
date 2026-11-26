// Serviço de teste: aloca 64 MB; com memory_max=16M o cgroup deve matá-lo (OOM, SIGKILL).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(void) {
    for (int i = 1; i <= 64; i++) { memset(malloc(1 << 20), 1, 1 << 20); }
    puts("[memhog] ERRO: alocou 64 MB sem ser morto"); return 0;
}
