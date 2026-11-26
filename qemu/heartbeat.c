// Serviço demo: 3 batidas e sai -> aegis-init reinicia (restart=always).
#include <stdio.h>
#include <unistd.h>
int main(void) {
    for (int i = 1; i <= 3; i++) { printf("[heartbeat] pid=%d batida %d\n", getpid(), i); fflush(stdout); sleep(1); }
    return 1;
}
