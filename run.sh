#!/bin/sh
# Startet ArcadeOS in QEMU mit UEFI (OVMF) wie einen echten PC. SSH: ssh -p 2222 -i ~/.ssh/aios_ed25519 root@localhost
# Serielle Konsole im Terminal, beenden mit Ctrl-A X.
# disk.img behaelt den Zustand (/state) zwischen Boots; ein neuer Build setzt ihn zurueck.
IMG="$(dirname "$0")/output/images"
DISK="${DISK:-$IMG/disk.img}"       # andere Platte: DISK=/pfad/zu/kopie.img ./run.sh
VARS="${VARS:-$IMG/ovmf-vars.fd}"   # UEFI-Variablen (wie NVRAM eines echten PCs)
[ -f "$VARS" ] || cp /usr/share/OVMF/OVMF_VARS_4M.fd "$VARS"
ACCEL=tcg; [ -w /dev/kvm ] && ACCEL=kvm
exec qemu-system-x86_64 -M pc -accel "$ACCEL" -m 256 -smp 2 \
    -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
    -drive if=pflash,format=raw,file="$VARS" \
    -drive file="$DISK",if=virtio,format=raw \
    -netdev user,id=n0,hostfwd=tcp:127.0.0.1:2222-:22 -device virtio-net-pci,netdev=n0 \
    -device i6300esb -action watchdog=reset \
    -nographic "$@"
