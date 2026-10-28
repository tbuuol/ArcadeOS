#!/usr/bin/env python3
"""Fragt eine laufende (oder haengende) ArcadeOS-VM ueber QMP ab (run.sh mit DEBUG_DIR).

    tools/qmp.py <qmp.sock> dump [stichproben]   Status, CPUs, Register aller Kerne, Befehlszeiger-Stichproben
    tools/qmp.py <qmp.sock> hmp "<befehl>"       beliebiger Monitor-Befehl, z.B. "info registers -a"
    tools/qmp.py <qmp.sock> screen <datei.png>   Bildschirmfoto (was ein angeschlossener Monitor zeigt)

Nur Standardbibliothek.
"""
import json
import os
import re
import socket
import struct
import sys
import time
import zlib


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


def identify(q, rip, workdir="/tmp"):
    """Welches UEFI-Modul (PE-Image) enthaelt diese Adresse? Nur fuer identitaetsabgebildete Adressen
    unter 4 GB (Firmware-Phase). Liest 2 MB unterhalb der Adresse per pmemsave, sucht rueckwaerts den
    MZ-Kopf auf einer 4-KB-Grenze und darin den Debug-Pfad (*.dll / *.efi / *.pdb)."""
    addr = int(rip, 16)
    if addr >= 1 << 32:
        return None
    size = 2 * 1024 * 1024
    start = max(0, (addr & ~0xFFF) - size + 0x1000)
    path = f"{workdir}/qmp-mem-{start:x}.bin"
    r = q.cmd("pmemsave", val=start, size=addr - start + 0x1000, filename=path)
    if "error" in r:
        return {"error": r["error"].get("desc")}
    data = open(path, "rb").read()
    for off in range((len(data) - 1) & ~0xFFF, -1, -0x1000):
        if data[off:off + 2] == b"MZ":
            img = data[off:]
            names = re.findall(rb"[\x20-\x7e]{6,200}\.(?:dll|efi|pdb)", img[:size])
            return {"image_base": hex(start + off), "offset": hex(addr - start - off),
                    "name": names[0].decode() if names else None}
    return {"image_base": None}


def ppm_to_png(src, dst):
    """P6-PPM (QEMU-screendump) -> PNG, nur Standardbibliothek"""
    data = open(src, "rb").read()
    tok, pos = [], 0
    while len(tok) < 4:                      # Kopf: P6 breite hoehe maxwert
        while data[pos:pos + 1].isspace():
            pos += 1
        end = pos
        while not data[end:end + 1].isspace():
            end += 1
        tok.append(data[pos:end]); pos = end
    pos += 1
    w, h = int(tok[1]), int(tok[2])
    px = data[pos:pos + w * h * 3]
    raw = b"".join(b"\0" + px[y * w * 3:(y + 1) * w * 3] for y in range(h))

    def chunk(kind, body):
        c = kind + body
        return struct.pack(">I", len(body)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)
    with open(dst, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    q = QMP(sys.argv[1])
    if sys.argv[2] == "hmp":
        print(q.hmp(sys.argv[3]))
        return
    if sys.argv[2] == "screen":
        # QEMU ohne libpng kann nur PPM; das Umwandeln in PNG geht auch ohne Zusatzpakete
        ppm = sys.argv[3] + ".ppm"
        r = q.cmd("screendump", filename=ppm)
        if "error" in r:
            print(r["error"].get("desc")); sys.exit(1)
        ppm_to_png(ppm, sys.argv[3])
        os.remove(ppm)
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
        print("== Module am Befehlszeiger (Firmware-Phase)")
        for cpu, rip in sorted(rips(q.hmp("info registers -a")).items()):
            try:
                print(cpu, rip, json.dumps(identify(q, rip)))
            except Exception as e:                  # noqa: BLE001 -- Diagnose darf nicht abbrechen
                print(cpu, rip, "Fehler:", e)
        print("== info pic"); print(q.hmp("info pic"))
        print("== info lapic"); print(q.hmp("info lapic"))
        return
    print(__doc__)
    sys.exit(2)


if __name__ == "__main__":
    main()
