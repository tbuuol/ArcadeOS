#!/usr/bin/env python3
"""Fragt eine laufende (oder haengende) ArcadeOS-VM ueber QMP ab (run.sh mit DEBUG_DIR).

    tools/qmp.py <qmp.sock> dump [stichproben]   Status, CPUs, Register aller Kerne, Befehlszeiger-Stichproben
    tools/qmp.py <qmp.sock> hmp "<befehl>"       beliebiger Monitor-Befehl, z.B. "info registers -a"

Nur Standardbibliothek.
"""
import json
import re
import socket
import sys
import time


class QMP:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.settimeout(10)
        self.s.connect(path)
        self.f = self.s.makefile("rw")
        json.loads(self.f.readline())                 # Begruessung
        self.cmd("qmp_capabilities")

    def cmd(self, name, **args):
        self.f.write(json.dumps({"execute": name, "arguments": args}) + "\n")
        self.f.flush()
        while True:
            msg = json.loads(self.f.readline())
            if "return" in msg or "error" in msg:   # Ereignisse ("event") ueberspringen
                return msg

    def hmp(self, line):
        r = self.cmd("human-monitor-command", **{"command-line": line})
        return r.get("return", r)


def rips(regs_text):
    """Befehlszeiger je CPU aus 'info registers -a'"""
    out, cpu = {}, None
    for line in regs_text.splitlines():
        m = re.match(r"CPU#(\d+)", line)
        if m:
            cpu = int(m.group(1))
        m = re.search(r"RIP=([0-9a-f]+)", line)
        if m and cpu is not None:
            out[cpu] = m.group(1)
        m = re.search(r"EIP=([0-9a-f]+)", line)
        if m and cpu is not None and cpu not in out:
            out[cpu] = m.group(1)
    return out


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    q = QMP(sys.argv[1])
    if sys.argv[2] == "hmp":
        print(q.hmp(sys.argv[3]))
        return
    if sys.argv[2] == "dump":
        n = int(sys.argv[3]) if len(sys.argv) > 3 else 10
        print("== query-status");  print(json.dumps(q.cmd("query-status").get("return")))
        print("== query-cpus-fast"); print(json.dumps(q.cmd("query-cpus-fast").get("return"), indent=1))
        print("== info registers -a"); print(q.hmp("info registers -a"))
        print("== Befehlszeiger-Stichproben (1 s Abstand)")
        for i in range(n):
            print(i, json.dumps(rips(q.hmp("info registers -a"))))
            time.sleep(1)
        print("== info pic"); print(q.hmp("info pic"))
        print("== info lapic"); print(q.hmp("info lapic"))
        return
    print(__doc__)
    sys.exit(2)


if __name__ == "__main__":
    main()
