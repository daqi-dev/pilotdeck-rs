/* Negative candidate: execv preserves PID, so fork/process-tree checks alone
 * cannot detect forwarding. Compile locally; never use it as a Rust result. */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char **args = calloc((size_t)argc + 2, sizeof(char *));
    if (!args) return 125;
    args[0] = NODE_PATH;
    args[1] = TS_ENTRY;
    for (int i = 1; i < argc; i++) args[i + 1] = argv[i];
    execv(NODE_PATH, args);
    int error = errno;
    free(args);
    if (error == EPERM || error == EACCES) {
        fputs("D0 exec-node denied\n", stderr);
        return 126;
    }
    fprintf(stderr, "D0 exec-node unexpected errno=%d\n", error);
    return 125;
}
