/* Native reporter: no language runtime may reset signals before we read them.
 * Built only in a test-owned temporary directory; no product helper/dependency.
 */
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int inherited(void) {
  struct sigaction action;
  sigset_t mask;
  if (sigaction(SIGINT, NULL, &action) || sigprocmask(SIG_SETMASK, NULL, &mask))
    return 2;
  printf("INT_IGN=%d TERM_BLOCKED=%d\n", action.sa_handler == SIG_IGN,
         sigismember(&mask, SIGTERM));
  return action.sa_handler == SIG_IGN && sigismember(&mask, SIGTERM) == 1 ? 0 : 3;
}

int main(int argc, char **argv) {
  if (argc >= 4 && (!strcmp(argv[1], "launch") || !strcmp(argv[1], "launch-all"))) {
    struct sigaction action = {0};
    sigset_t mask;
    action.sa_handler = SIG_IGN;
    if (sigemptyset(&action.sa_mask) || sigemptyset(&mask)) return 4;
    for (int sig = 1; sig < NSIG; ++sig) {
      if (sig == SIGKILL || sig == SIGSTOP) continue;
#ifdef __linux__
      if (sig == 32 || sig == 33) continue; /* glibc-private realtime signals */
#endif
      int all = !strcmp(argv[1], "launch-all");
      /* Ignoring CHLD would auto-reap the Rust worker's children. Blocking it
       * still exercises mask reset without changing wait semantics. */
      if ((sig == SIGINT || (all && sig < 32 && sig != SIGCHLD)) &&
          sigaction(sig, &action, NULL)) return 5;
      if ((sig == SIGTERM || all) && sigaddset(&mask, sig)) return 6;
    }
    if (sigprocmask(SIG_SETMASK, &mask, NULL)) return 7;
    if (inherited()) return 8;
    fflush(stdout);
    execv(argv[2], &argv[2]);
    perror("execv worker");
    return 9;
  }
  if (argc == 2 && !strcmp(argv[1], "inherited")) return inherited();
  /* Use the reporter as a configured hook shell, before any shell runtime
   * can change dispositions; stdio transports can collect via a file. */
  if (argc == 3 && !strcmp(argv[1], "report-file")) {
    if (!freopen(argv[2], "w", stdout)) return 15;
  } else if (!((argc == 2 && !strcmp(argv[1], "report")) ||
               (argc == 3 && !strcmp(argv[1], "-c") && !strcmp(argv[2], "report")))) {
    return 10;
  }
  sigset_t mask;
  if (sigprocmask(SIG_SETMASK, NULL, &mask)) return 11;
  int count = 0, failed = 0, blocked = 0;
  for (int sig = 1; sig < NSIG; ++sig) {
    if (sig == SIGKILL || sig == SIGSTOP) continue;
#ifdef __linux__
    if (sig == 32 || sig == 33) continue;
#endif
    if (sig < 32) {
      struct sigaction action;
      if (sigaction(sig, NULL, &action)) return 12;
      printf("signal=%d default=%d blocked=%d\n", sig,
             action.sa_handler == SIG_DFL, sigismember(&mask, sig));
      failed |= action.sa_handler != SIG_DFL;
      ++count;
    }
    /* Check the entire public mask, including Linux realtime signals. */
    blocked |= sigismember(&mask, sig) != 0;
  }
#ifdef __linux__
  FILE *status = fopen("/proc/self/status", "r");
  if (!status) return 13;
  char line[256];
  while (fgets(line, sizeof(line), status)) {
    if (!strncmp(line, "SigIgn:", 7) || !strncmp(line, "SigBlk:", 7))
      fputs(line, stdout);
  }
  fclose(status);
#endif
  printf("checked=%d mask_empty=%d\n", count, !blocked);
  return count == 29 && !failed && !blocked ? 0 : 14;
}
