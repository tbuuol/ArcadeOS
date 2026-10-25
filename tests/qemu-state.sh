#!/bin/sh
# Testet read-only Rootfs + Zustands-Partition in QEMU. Arbeitet auf Kopien von disk.img.
# Aufruf: tests/qemu-state.sh [ssh-private-key]   (Standard: ~/.ssh/aios_ed25519)
# Braucht Port 2222 frei; stoppt am Ende alle qemu-system-x86-Prozesse des Users.
SP=$(mktemp -d)
trap 'pkill -x qemu-system-x86; rm -rf "$SP"' EXIT
IMG="$(cd "$(dirname "$0")/.." && pwd)/output/images"
KEY=${1:-$HOME/.ssh/aios_ed25519}
# timeout + Keepalive: bricht die VM mitten in einer Verbindung weg, haengt ssh sonst minutenlang
S="timeout 60 ssh -p 2222 -i $KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=3 -o ServerAliveInterval=2 -o ServerAliveCountMax=3 -o BatchMode=yes root@127.0.0.1"
PASS=0; FAIL=0
ok()   { echo "  OK   $*"; PASS=$((PASS+1)); }
bad()  { echo "  FAIL $*"; FAIL=$((FAIL+1)); }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
boot() {  # $1 = disk image
    T0=$(date +%s.%N)
    DISK="$1" VARS="$SP/vars.fd" "$ROOT/run.sh" -serial file:$SP/serial.log -monitor none -display none >/dev/null 2>&1 &
    for i in $(seq 1 180); do
        $S true 2>/dev/null && { BOOT=$(echo "$(date +%s.%N) - $T0" | bc | cut -c1-5); return 0; }
        sleep 0.5
    done
    BOOT=timeout; return 1
}
# grubenv direkt im Image lesen/aendern (Datei liegt zusammenhaengend in der FAT-Partition)
grubenv() {  # $1 = image, $2 = neuer Inhalt (optional, Zeilen durch \n getrennt)
    python3 -I - "$1" "${2:-}" <<'PY'
import sys
p, new = sys.argv[1], sys.argv[2]
d = bytearray(open(p, 'rb').read())
# Der Kopf steht auch als Text im GRUB-Programm: nur einen Block nehmen, der ganz aus Text besteht
import re
i = next((m.start() for m in re.finditer(b"# GRUB Environment Block\n", d)
          if all(c == 10 or 32 <= c < 127 for c in d[m.start():m.start()+1024])), -1)
assert i >= 0, "grubenv nicht gefunden"
if new:
    blk = (b"# GRUB Environment Block\n" + new.encode().decode('unicode_escape').encode() + b"#" * 1024)[:1024]
    d[i:i+1024] = blk
    open(p, 'wb').write(d)
print(d[i:i+1024].split(b"\n#")[0].decode().replace("# GRUB Environment Block\n", "").replace("\n", " "))
PY
}
halt_vm() { $S 'sync; /etc/init.d/S00state stop' >/dev/null 2>&1; pkill -x qemu-system-x86; sleep 1; }
hostkey() { ssh-keyscan -p 2222 -t ed25519 127.0.0.1 2>/dev/null | awk '{print $3}' | sha256sum | cut -c1-16; }

cp "$IMG/disk.img" $SP/disk-test.img

echo "== Boot 1 (frisches Image)"
boot $SP/disk-test.img && ok "SSH nach ${BOOT}s" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }
$S 'cat /proc/cmdline' | grep -q 'arcadeos.slot=A' && ok "UEFI -> GRUB -> Slot A" || bad "cmdline: $($S 'cat /proc/cmdline')"
$S 'grep " / " /proc/mounts' | grep -q 'squashfs ro' && ok "/ ist squashfs ro" || bad "/ nicht ro: $($S 'grep " / " /proc/mounts')"
$S 'touch /probe 2>/dev/null' && bad "Schreiben auf / moeglich" || ok "Schreiben auf / abgelehnt"
$S 'grep " /state " /proc/mounts' | grep -q ext4 && ok "/state ist ext4" || bad "/state: $($S 'grep " /state " /proc/mounts')"
J=$($S 'cat /run/state.json'); echo "$J" | grep -q '"mode":"persistent"' && ok "state.json: $J" || bad "state.json: $J"
$S 'ls /state/dropbear' | grep -q ed25519 && ok "Host-Keys liegen in /state/dropbear" || bad "keine Host-Keys in /state"
K1=$(hostkey)
$S 'echo hallo > /state/probe' && ok "Schreiben auf /state" || bad "Schreiben auf /state"
$S 'df -h / /state | tail -2'
halt_vm

echo "== Boot 2 (gleiches Image, Zustand muss bleiben)"
boot $SP/disk-test.img && ok "SSH nach ${BOOT}s" || bad "kein SSH"
K2=$(hostkey)
[ "$K1" = "$K2" ] && ok "Host-Key unveraendert ($K1)" || bad "Host-Key geaendert ($K1 -> $K2)"
[ "$($S 'cat /state/probe')" = hallo ] && ok "Datei auf /state ueberlebt Neustart" || bad "Datei auf /state weg"
$S 'ls /state/seedrng' | grep -q seed && ok "Zufalls-Seed gespeichert" || bad "kein Seed in /state/seedrng"
halt_vm

echo "== Boot 3 (Zustands-Partition zerstoert -> Fallback)"
cp "$IMG/disk.img" $SP/disk-broken.img
START=$(python3 -I - $SP/disk-broken.img <<'PY'
# Startsektor der GPT-Partition mit dem Namen "state" (nicht ueber die Position)
import struct, sys
d = open(sys.argv[1], 'rb').read(1024 * 1024)
lba, n, size = struct.unpack('<QII', d[512+72:512+88])
for i in range(n):
    e = d[lba*512 + i*size : lba*512 + (i+1)*size]
    if e[56:128].decode('utf-16-le').rstrip('\0') == 'state':
        print(struct.unpack('<Q', e[32:40])[0]); break
PY
)
dd if=/dev/zero of=$SP/disk-broken.img bs=512 seek="$START" count=8192 conv=notrunc 2>/dev/null
echo "  (state-Partition ab Sektor $START genullt)"
boot $SP/disk-broken.img && ok "SSH trotz kaputter Zustands-Partition nach ${BOOT}s" || bad "kein SSH bei kaputter Partition"
J=$($S 'cat /run/state.json'); echo "$J" | grep -q '"mode":"ephemeral"' && ok "state.json: $J" || bad "state.json: $J"
$S 'grep " /state " /proc/mounts' | grep -q tmpfs && ok "/state ist tmpfs" || bad "/state kein tmpfs"
halt_vm

echo "== Boot 4 (Probeboot auf leeren Slot B -> Fallback auf A)"
cp "$IMG/disk.img" $SP/disk-try.img
echo "  grubenv vorher:  $(grubenv $SP/disk-try.img 'slot=A\ntry_slot=B\n')"
boot $SP/disk-try.img && ok "SSH nach ${BOOT}s" || bad "kein SSH nach Probeboot"
$S 'cat /proc/cmdline' | grep -q 'arcadeos.slot=A' && ok "laeuft auf Slot A (Fallback)" || bad "cmdline: $($S 'cat /proc/cmdline')"
halt_vm
ENV=$(grubenv $SP/disk-try.img); echo "  grubenv nachher: $ENV"
echo "$ENV" | grep -q 'try_slot=B' && bad "try_slot nicht geloescht" || ok "Probeboot verbraucht (try_slot geloescht)"
echo "$ENV" | grep -q 'slot=A' && ok "fester Slot bleibt A" || bad "fester Slot: $ENV"

echo "== Ergebnis: $PASS ok, $FAIL fehlgeschlagen"
[ $FAIL = 0 ]
