#!/bin/sh
# Testet read-only Rootfs + Zustands-Partition in QEMU. Arbeitet auf Kopien von disk.img.
# Aufruf: tests/qemu-state.sh [ssh-private-key]   (Standard: ~/.ssh/aios_ed25519)
# Braucht Port 2222 frei; stoppt am Ende alle qemu-system-x86-Prozesse des Users.
SP=$(mktemp -d)
trap 'pkill -x qemu-system-x86; rm -rf "$SP"' EXIT
IMG="$(cd "$(dirname "$0")/.." && pwd)/output/images"
KEY=${1:-$HOME/.ssh/aios_ed25519}
S="ssh -p 2222 -i $KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=3 -o BatchMode=yes root@127.0.0.1"
PASS=0; FAIL=0
ok()   { echo "  OK   $*"; PASS=$((PASS+1)); }
bad()  { echo "  FAIL $*"; FAIL=$((FAIL+1)); }

boot() {  # $1 = disk image
    T0=$(date +%s.%N)
    qemu-system-x86_64 -M pc -accel tcg -m 256 -smp 2 -kernel "$IMG/bzImage" \
        -drive file="$1",if=virtio,format=raw \
        -append "rootwait root=PARTLABEL=rootfs-a rootfstype=squashfs ro console=ttyS0 quiet" \
        -netdev user,id=n0,hostfwd=tcp:127.0.0.1:2222-:22 -device virtio-net-pci,netdev=n0 \
        -display none -monitor none -serial file:$SP/serial.log >/dev/null 2>&1 &
    for i in $(seq 1 120); do
        $S true 2>/dev/null && { BOOT=$(echo "$(date +%s.%N) - $T0" | bc | cut -c1-5); return 0; }
        sleep 0.5
    done
    BOOT=timeout; return 1
}
halt_vm() { $S 'sync; /etc/init.d/S00state stop' >/dev/null 2>&1; pkill -x qemu-system-x86; sleep 1; }
hostkey() { ssh-keyscan -p 2222 -t ed25519 127.0.0.1 2>/dev/null | awk '{print $3}' | sha256sum | cut -c1-16; }

cp "$IMG/disk.img" $SP/disk-test.img

echo "== Boot 1 (frisches Image)"
boot $SP/disk-test.img && ok "SSH nach ${BOOT}s" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }
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
START=$(sgdisk -i 3 $SP/disk-broken.img 2>/dev/null | sed -n 's/^First sector: \([0-9]*\).*/\1/p')
[ -n "$START" ] || START=$(python3 -I -c "
import struct,sys
d=open('$SP/disk-broken.img','rb').read(1024*1024)
lba=struct.unpack('<Q',d[512+72:512+80])[0]; e=d[lba*512+2*128:lba*512+3*128]
print(struct.unpack('<Q',e[32:40])[0])")
dd if=/dev/zero of=$SP/disk-broken.img bs=512 seek="$START" count=8192 conv=notrunc 2>/dev/null
echo "  (state-Partition ab Sektor $START genullt)"
boot $SP/disk-broken.img && ok "SSH trotz kaputter Zustands-Partition nach ${BOOT}s" || bad "kein SSH bei kaputter Partition"
J=$($S 'cat /run/state.json'); echo "$J" | grep -q '"mode":"ephemeral"' && ok "state.json: $J" || bad "state.json: $J"
$S 'grep " /state " /proc/mounts' | grep -q tmpfs && ok "/state ist tmpfs" || bad "/state kein tmpfs"
halt_vm

echo "== Ergebnis: $PASS ok, $FAIL fehlgeschlagen"
[ $FAIL = 0 ]
