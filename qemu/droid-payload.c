// Payload de teste do container (substituto do Android até a etapa 6).
// Prova isolamento (PID 1 no namespace, hostname próprio), consumo de RAM (8 MB) e freeze (ticks param).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
char *volatile keep; // volatile: impede o GCC de eliminar malloc+memset sem uso
int main(void) {
    char host[64] = {0};
    gethostname(host, sizeof host - 1);
    printf("[droid] pid no namespace=%d hostname=%s\n", getpid(), host);
    keep = malloc(8 << 20); memset(keep, 1, 8 << 20);
    for (int i = 1;; i++) { printf("[droid] tick %d\n", i); fflush(stdout); sleep(1); }
}
