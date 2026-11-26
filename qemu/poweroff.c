// Serviço de teste: espera N segundos e pede poweroff ao PID 1 (SIGTERM).
#include <signal.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) { sleep(argc > 1 ? atoi(argv[1]) : 5); return kill(1, SIGTERM); }
