/* yb-mirror menu-bar app launcher.
 *
 * A compiled executable rather than a shell script: this macOS refuses to
 * LaunchServices-open bundles whose CFBundleExecutable is a script
 * (open fails with -10669 regardless of signing), but happily launches
 * this. All it does is cd to the repo and exec uv run, so the app still
 * runs entirely inside the project's venv — no system python anywhere.
 *
 * Built by mac/make-app.sh; the repo path is baked in at compile time.
 */
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    const char *repo = REPO_PATH; /* injected: -DREPO_PATH='"..."' */
    const char *log = "/tmp/yb-mirror-menubar.log";
    /* Keep stdout/stderr across exec: a GUI app launched by LaunchServices
     * has neither usefully attached, so Python tracebacks (a frozen menu
     * state line once stayed undiagnosed for exactly this reason) would
     * vanish. freopen'd descriptors survive execl. */
    if (!freopen(log, "a", stdout)) {}
    if (!freopen(log, "a", stderr)) {}
    if (chdir(repo) != 0) {
        fprintf(stderr, "yb-mirror: repo moved? not found at %s "
                        "(rebuild with mac/make-app.sh)\n", repo);
        return 1;
    }
    /* uv first from its usual homes, then from PATH */
    execl("/opt/homebrew/bin/uv", "uv", "run", "mac/menubar.py",
          (char *)NULL);
    execl("/usr/local/bin/uv", "uv", "run", "mac/menubar.py",
          (char *)NULL);
    execlp("uv", "uv", "run", "mac/menubar.py", (char *)NULL);
    fprintf(stderr, "yb-mirror: uv not found\n");
    return 127;
}
