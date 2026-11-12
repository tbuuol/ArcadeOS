/*
 * runas – ein Programm als anderer Benutzer starten (busybox setpriv kann keine UIDs setzen,
 * /etc/passwd ist read-only).
 *
 *   runas [-b] <uid> <gid> <programm> [argumente ...]
 *
 * -b: CAP_NET_BIND_SERVICE als Ambient-Capability behalten (Ports < 1024, z.B. Webserver auf 80/443),
 *     sonst keinerlei Rechte. Zusatzgruppen werden geleert, no_new_privs gesetzt.
 */
#define _GNU_SOURCE
#include <grp.h>
#include <linux/capability.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

static void die(const char *m) { perror(m); exit(127); }

static void caps(unsigned keep) {
	struct __user_cap_header_struct h = { _LINUX_CAPABILITY_VERSION_3, 0 };
	struct __user_cap_data_struct d[2];
	memset(d, 0, sizeof d);
	d[0].permitted = d[0].effective = d[0].inheritable = keep;
	if (syscall(SYS_capset, &h, d)) die("capset");
}

int main(int argc, char **argv) {
	int bind = 0, i = 1;
	if (argc > 1 && !strcmp(argv[1], "-b")) { bind = 1; i++; }
	if (argc - i < 3) { fprintf(stderr, "Aufruf: runas [-b] <uid> <gid> <programm> [argumente ...]\n"); return 2; }
	char *e1, *e2;
	long uid = strtol(argv[i], &e1, 10), gid = strtol(argv[i + 1], &e2, 10);
	if (*e1 || *e2 || uid <= 0 || gid <= 0) { fprintf(stderr, "runas: uid/gid muessen Zahlen > 0 sein\n"); return 2; }
	unsigned keep = bind ? (1u << CAP_NET_BIND_SERVICE) : 0;
	if (bind && prctl(PR_SET_KEEPCAPS, 1, 0, 0, 0)) die("keepcaps");
	if (setgroups(0, NULL)) die("setgroups");
	if (setresgid(gid, gid, gid)) die("setgid");
	if (setresuid(uid, uid, uid)) die("setuid");
	caps(keep);
	if (bind && prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, CAP_NET_BIND_SERVICE, 0, 0)) die("ambient");
	if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)) die("no_new_privs");
	execvp(argv[i + 2], argv + i + 2);
	die(argv[i + 2]);
}
