// Daedalus.app entry point. The product is a TUI, so a Finder launch hands the
// bundled CLI to Terminal instead of drawing into a windowless process.
#include <limits.h>
#include <mach-o/dyld.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char executable[PATH_MAX];
    uint32_t size = (uint32_t)sizeof(executable);
    if (_NSGetExecutablePath(executable, &size) != 0) {
        return 1;
    }
    char resolved[PATH_MAX];
    if (realpath(executable, resolved) == NULL) {
        return 1;
    }
    char *slash = strrchr(resolved, '/');
    if (slash == NULL) {
        return 1;
    }
    *slash = '\0';
    char command[PATH_MAX];
    int written = snprintf(
        command,
        sizeof(command),
        "%s/../Resources/Daedalus.command",
        resolved
    );
    if (written < 0 || (size_t)written >= sizeof(command)) {
        return 1;
    }
    execl("/usr/bin/open", "open", "-a", "Terminal", command, (char *)NULL);
    return 1;
}
