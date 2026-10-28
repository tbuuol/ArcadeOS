#!/bin/sh
# Startet ArcadeOS in QEMU mit UEFI (OVMF) wie einen echten PC. SSH: ssh -p 2222 -i ~/.ssh/aios_ed25519 root@localhost
# Serielle Konsole im Terminal, beenden mit Ctrl-A X.
# disk.img behaelt den Zustand (/state) zwischen Boots; ein neuer Build setzt ihn zurueck.
IMG="$(dirname "$0")/output/images"
DISK="${DISK:-$IMG/disk.img}"       # andere Platte: DISK=/pfad/zu/kopie.img ./run.sh
VARS="${VARS:-$IMG/ovmf-vars.fd}"   # UEFI-Variablen (wie NVRAM eines echten PCs)
SSH_PORT="${SSH_PORT:-2222}"        # SSH der VM auf localhost:SSH_PORT
SMP="${SMP:-2}"                     # Zahl der virtuellen CPU-Kerne
# Wie haengt die Platte am Rechner? virtio (Standard) | usb (Stick an xHCI) | uas | ahci (SATA) | nvme | scsi
DISK_BUS="${DISK_BUS:-virtio}"
# Firmware: uefi (OVMF, Standard) | bios (SeaBIOS, wie ein alter PC ohne UEFI)
FIRMWARE="${FIRMWARE:-uefi}"
# Netzwerkkarte: virtio-net-pci (Standard) | e1000 | e1000e | igb | vmxnet3 | usb (CDC-Ethernet-Adapter)
NIC="${NIC:-virtio-net-pci}"
ACCEL=tcg; [ -w /dev/kvm ] && ACCEL=kvm
# QEMU: standardmaessig der eigene Build (output-tools, QEMU 10.2), sonst das System-QEMU.
# Ubuntus QEMU 8.2.2 haengt/stuerzt ohne KVM bei ~3 % der Neustarts ab (siehe PLAN.md, 2026-10-26).
OWNQEMU="$(dirname "$0")/output-tools/host/bin/qemu-system-x86_64"
[ -x "$OWNQEMU" ] && QEMU="${QEMU:-$OWNQEMU}"
QEMU="${QEMU:-qemu-system-x86_64}"
# TCG_THREAD=single: alle virtuellen Kerne in einem Host-Thread (statt einem pro Kern), zur Diagnose
[ "$ACCEL" = tcg ] && [ -n "${TCG_THREAD:-}" ] && ACCEL="tcg,thread=$TCG_THREAD"
# Diagnose: DEBUG_DIR=/pfad ./run.sh schreibt dorthin
#   qemu.log  CPU-Resets (voller Registerstand), Gastfehler, nicht implementierte Funktionen
#   ovmf.log  Ausgabe der UEFI-Firmware (Debug-Port 0x402)
#   qmp.sock  Steuerkanal: tools/qmp.py fragt eine laufende/haengende VM ab
DEBUG_ARGS=""
if [ -n "${DEBUG_DIR:-}" ]; then
    mkdir -p "$DEBUG_DIR"
    DEBUG_ARGS="-chardev file,id=ovmfdbg,path=$DEBUG_DIR/ovmf.log -device isa-debugcon,iobase=0x402,chardev=ovmfdbg
        -qmp unix:$DEBUG_DIR/qmp.sock,server=on,wait=off"
    # DEBUG_QEMU_LOG=0: ohne QEMU-Log (das Protokollieren kann das Timing veraendern)
    [ "${DEBUG_QEMU_LOG:-1}" = 1 ] && DEBUG_ARGS="-d cpu_reset,guest_errors,unimp -D $DEBUG_DIR/qemu.log $DEBUG_ARGS"
fi
case "$DISK_BUS" in
    virtio) DISK_ARGS="-drive file=$DISK,if=virtio,format=raw" ;;
    usb)    DISK_ARGS="-device qemu-xhci,id=xhci -drive file=$DISK,if=none,id=d0,format=raw -device usb-storage,bus=xhci.0,drive=d0" ;;
    uas)    DISK_ARGS="-device qemu-xhci,id=xhci -device usb-uas,id=uas,bus=xhci.0 -drive file=$DISK,if=none,id=d0,format=raw -device scsi-hd,bus=uas.0,scsi-id=0,lun=0,drive=d0" ;;
    ahci)   DISK_ARGS="-device ich9-ahci,id=ahci -drive file=$DISK,if=none,id=d0,format=raw -device ide-hd,bus=ahci.0,drive=d0" ;;
    nvme)   DISK_ARGS="-drive file=$DISK,if=none,id=d0,format=raw -device nvme,drive=d0,serial=arcadeos0" ;;
    scsi)   DISK_ARGS="-device virtio-scsi-pci,id=scsi -drive file=$DISK,if=none,id=d0,format=raw -device scsi-hd,bus=scsi.0,drive=d0" ;;
    *) echo "DISK_BUS=$DISK_BUS unbekannt" >&2; exit 2 ;;
esac
case "$NIC" in
    usb) NIC_DEV="-device qemu-xhci,id=xhcinet -device usb-net,bus=xhcinet.0,netdev=n0" ;;
    *)   NIC_DEV="-device $NIC,netdev=n0" ;;
esac
if [ "$FIRMWARE" = bios ]; then
    FW_ARGS=""          # ohne pflash startet QEMU mit SeaBIOS
else
    [ -f "$VARS" ] || cp /usr/share/OVMF/OVMF_VARS_4M.fd "$VARS"
    FW_ARGS="-drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd -drive if=pflash,format=raw,file=$VARS"
fi
exec "$QEMU" -M pc -accel "$ACCEL" -m 256 -smp "$SMP" \
    $FW_ARGS \
    $DISK_ARGS \
    -netdev user,id=n0,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22 $NIC_DEV \
    -device i6300esb -action watchdog=reset \
    $DEBUG_ARGS -nographic "$@"
