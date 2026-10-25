#!/bin/sh
# Startet ArcadeOS in QEMU. SSH: ssh -p 2222 -i ~/.ssh/aios_ed25519 root@localhost
# Serielle Konsole im Terminal, beenden mit Ctrl-A X.
# disk.img behaelt den Zustand (/state) zwischen Boots; ein neuer Build setzt ihn zurueck.
IMG="$(dirname "$0")/output/images"
ACCEL=tcg; [ -w /dev/kvm ] && ACCEL=kvm
exec qemu-system-x86_64 -M pc -accel "$ACCEL" -m 256 -smp 2 \
    -kernel "$IMG/bzImage" \
    -drive file="$IMG/disk.img",if=virtio,format=raw \
    -append "rootwait root=PARTLABEL=rootfs-a rootfstype=squashfs ro console=ttyS0 quiet" \
    -netdev user,id=n0,hostfwd=tcp:127.0.0.1:2222-:22 -device virtio-net-pci,netdev=n0 \
    -nographic "$@"
