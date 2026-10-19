// Keep a grantable responsible app alive for the server and its replacements.
// See herdr#808. Executing the server in this process would discard that identity.
#include <errno.h>
#include <dlfcn.h>
#include <signal.h>
#include <spawn.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t server_pid;
static volatile sig_atomic_t server_result;
extern char **environ;

static int become_responsible(char **argv) {
    const char *marker = "HERDR_APP_RESPONSIBILITY_READY";
    if (getenv(marker) != NULL) {
        unsetenv(marker);
        return 0;
    }
    // Re-exec the bundled helper as its own responsible process. A forked
    // child of the finished launchd job is otherwise left without an owner.
    // Resolve the macOS spawn API at runtime, as LLDB does.
    typedef int (*disclaim_fn)(posix_spawnattr_t *, bool);
    disclaim_fn disclaim = (disclaim_fn)dlsym(RTLD_DEFAULT, "responsibility_spawnattrs_setdisclaim");
    if (disclaim == NULL) { fprintf(stderr, "macOS responsibility API unavailable\n"); return 1; }
    posix_spawnattr_t attrs;
    int error = posix_spawnattr_init(&attrs);
    if (error != 0) { errno = error; perror("spawn attributes"); return 1; }
    error = posix_spawnattr_setflags(&attrs, POSIX_SPAWN_SETEXEC);
    if (error == 0) error = disclaim(&attrs, true);
    if (error == 0 && setenv(marker, "1", 1) < 0) error = errno;
    if (error == 0) error = posix_spawn(NULL, argv[0], NULL, &attrs, argv, environ);
    posix_spawnattr_destroy(&attrs);
    errno = error;
    perror("establish responsible app");
    return 1;
}

static void child_changed(int signal_number) {
    (void)signal_number;
    int saved_errno = errno;
    int status;
    pid_t pid = server_pid;
    if (pid > 0 && waitpid(pid, &status, WNOHANG) == pid) {
        server_result = WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
        server_pid = 0;
    }
    errno = saved_errno;
}

static void forward_signal(int signal_number) {
    int saved_errno = errno;
    if (server_pid > 0) kill(server_pid, signal_number);
    errno = saved_errno;
}

int main(int argc, char **argv) {
    if (argc < 2 || argv[1][0] != '/') {
        fprintf(stderr, "usage: herdr-server-launcher /absolute/path/to/herdr [arguments...]\n");
        return 2;
    }

    if (getenv("HERDR_LAUNCHD_DETACH") != NULL) {
        unsetenv("HERDR_LAUNCHD_DETACH");
        setenv("XPC_SERVICE_NAME", "0", 1);
        // Leave the GUI job as before, but retain a bundle identity after its exit.
        pid_t detached = fork();
        if (detached < 0) { perror("fork launcher"); return 1; }
        if (detached > 0) _exit(0);
        if (setsid() < 0) { perror("setsid launcher"); return 1; }
        // The job's original process must exit before a server inherits its
        // replacement's responsibility. Otherwise it can inherit a dead owner.
        while (getppid() != 1) usleep(1000);
    }

    if (become_responsible(argv) != 0) return 1;

    int lifetime[2];
    if (pipe(lifetime) < 0) { perror("pipe"); return 1; }
    struct sigaction action = {0};
    sigemptyset(&action.sa_mask);
    action.sa_handler = child_changed;
    if (sigaction(SIGCHLD, &action, NULL) < 0) { perror("sigaction"); return 1; }
    action.sa_handler = forward_signal;
    if (sigaction(SIGTERM, &action, NULL) < 0 || sigaction(SIGINT, &action, NULL) < 0) {
        perror("sigaction"); return 1;
    }

    sigset_t blocked, previous;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGCHLD);
    if (sigprocmask(SIG_BLOCK, &blocked, &previous) < 0) { perror("sigprocmask"); return 1; }
    pid_t child = fork();
    if (child < 0) { perror("fork server"); return 1; }
    if (child == 0) {
        close(lifetime[0]);
        if (sigprocmask(SIG_SETMASK, &previous, NULL) < 0) { perror("sigprocmask"); _exit(1); }
        if (setsid() < 0) { perror("setsid server"); _exit(1); }
        // Deliberately inherit the writer through exec, including live handoff.
        // EOF means no remaining process needs this app's responsible identity.
        execv(argv[1], argv + 1);
        perror("exec server");
        _exit(127);
    }
    server_pid = child;
    if (sigprocmask(SIG_SETMASK, &previous, NULL) < 0) { perror("sigprocmask"); return 1; }
    close(lifetime[1]);
    for (;;) {
        char unused;
        ssize_t received = read(lifetime[0], &unused, sizeof(unused));
        if (received == 0) break;
        if (received < 0 && errno != EINTR) { perror("read lifetime"); return 1; }
    }
    close(lifetime[0]);
    if (sigprocmask(SIG_BLOCK, &blocked, &previous) < 0) { perror("sigprocmask"); return 1; }
    while (server_pid > 0) sigsuspend(&previous);
    return server_result;
}
