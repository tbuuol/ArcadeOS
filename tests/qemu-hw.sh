#!/bin/sh
# Echte-Hardware-Test in QEMU: dasselbe disk.img mit UEFI und altem BIOS, an verschiedenen Anschluessen (USB-Stick, UAS, SATA,
# NVMe, SCSI) mit verschiedenen Netzwerkkarten (Intel e1000/e1000e/igb, VMware, USB-Adapter).
# Je Kombination: bootet es, liegt die Rootfs auf dem erwarteten Geraet, ist /state persistent,
# hat die Karte den richtigen Treiber, und zeigt der Bildschirm die Statusseite mit der IP.
# Bildschirmfotos: test-logs/qemu-hw-screens/
# Aufruf: tests/qemu-hw.sh [ssh-key]
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KEY=${1:-}; [ -n "$KEY" ] || unset KEY
. "$ROOT/tests/lib.sh"
SHOTS="$ROOT/test-logs/qemu-hw-screens"; mkdir -p "$SHOTS"

# firmware platte netzwerkkarte erwartetes-geraet erwarteter-treiber
COMBOS="uefi:usb:e1000:sda:e1000
uefi:usb+uas:e1000e:sd:e1000e
uefi:ahci:igb:sda:igb
uefi:nvme:vmxnet3:nvme0n1:vmxnet3
uefi:scsi:usb:sda:cdc_ether
bios:usb:e1000e:sda:e1000e
bios:ahci:e1000:sda:e1000
bios:nvme:igb:nvme0n1:igb
bios:virtio:virtio-net-pci:vda:virtio_net"

for c in $COMBOS; do
    IFS=: read -r fw bus nic dev drv <<X
$c
X
    echo "== Firmware $fw, Platte $bus, Netzwerkkarte $nic"
    rm -f $SP/disk.img $SP/vars.fd
    cp "$IMG/disk.img" $SP/disk.img
    T0=$(date +%s)
    # usb+uas: Start vom USB-Stick, dazu eine Datenplatte per UAS. Firmware (OVMF/SeaBIOS) kann
    # nur das klassische USB-Speicherprotokoll (BOT); echte UAS-Gehaeuse beherrschen beides.
    EXTRA=""
    if [ "$bus" = usb+uas ]; then
        bus=usb; truncate -s 64M $SP/uas.img
        EXTRA="-device usb-uas,id=uas,bus=xhci.0 -drive file=$SP/uas.img,if=none,id=d1,format=raw -device scsi-hd,bus=uas.0,scsi-id=0,lun=0,drive=d1"
    fi
    FIRMWARE=$fw DISK_BUS=$bus NIC=$nic start_vm $EXTRA
    if ! wait_ssh; then bad "$fw/$bus/$nic: kein SSH"; kill $VM_PID 2>/dev/null; wait $VM_PID 2>/dev/null; continue; fi
    ok "$fw/$bus/$nic: SSH nach $(( $(date +%s) - T0 )) s"
    root=$($S 'for u in /sys/class/block/*/uevent; do grep -qx PARTNAME=rootfs-a $u && sed -n s/^DEVNAME=//p $u; done')
    case "$root" in "$dev"*) ok "Rootfs auf $root";; *) bad "Rootfs auf '$root', erwartet $dev*";; esac
    if [ -n "$EXTRA" ]; then
        u=$($S 'for b in /sys/block/sd*; do echo ${b##*/} $(cat $b/size); done' | awk '$2==131072{print $1}')
        [ -n "$u" ] && ok "UAS-Platte erkannt (/dev/$u, 64 MB)" || bad "UAS-Platte fehlt"
        $S 'ls /sys/bus/usb/drivers/uas/ | grep -q :' && ok "Treiber uas gebunden" || bad "uas nicht gebunden"
    fi
    check "$($S cat /run/state.json | jfield mode)" persistent "Zustands-Partition gemountet"
    nd=""
    for n in $($S ls /sys/class/net); do
        [ "$n" = lo ] && continue
        d=$($S readlink /sys/class/net/$n/device/driver); nd="$nd ${d##*/}"
    done
    case "$nd" in *" $drv"*) ok "Netzwerkkarte:$nd";; *) bad "Treiber '$nd', erwartet $drv";; esac
    ip=$($S "ip -o -4 addr show scope global" | awk '{print $4}' | head -1)
    [ -n "$ip" ] && ok "DHCP-Adresse $ip" || bad "keine IPv4-Adresse"
    # Statusseite: als Text und auf dem (virtuellen) Bildschirm
    txt=$($S /usr/lib/arcadeos/status-screen --once)
    echo "$txt" | grep -q "ssh root@${ip%/*}" && ok "Statusseite nennt SSH-Befehl" || bad "Statusseite ohne SSH-Befehl: $txt"
    echo "$txt" | grep -q "Key:   SHA256:" && ok "Statusseite nennt Fingerabdruck" || bad "kein Fingerabdruck auf der Statusseite"
    hk=$($S "dropbearkey -y -f /etc/dropbear/dropbear_ed25519_host_key" | sed -n 's/^Fingerprint: *//p')
    echo "$txt" | grep -qF "$hk" && ok "Fingerabdruck = Host-Key von dropbear" || bad "Fingerabdruck passt nicht: $hk"
    # UEFI: Grafikbild der Firmware (efifb); BIOS: VGA-Textmodus
    if [ $fw = uefi ]; then
        [ "$($S 'test -c /dev/fb0 && echo ja')" = ja ] && ok "Bildschirm: Grafik (/dev/fb0)" || bad "kein /dev/fb0"
    else
        $S 'cat /sys/class/vtconsole/*/name' | grep -q VGA && ok "Bildschirm: VGA-Text" || bad "keine VGA-Konsole"
    fi
    sleep 6   # einmal neu zeichnen lassen
    python3 -I "$ROOT/tools/qmp.py" "$SP/debug/qmp.sock" screen "$SHOTS/$fw-$bus-$nic.png" \
        && [ -s "$SHOTS/$fw-$bus-$nic.png" ] && ok "Bildschirmfoto $fw-$bus-$nic.png" || bad "kein Bildschirmfoto"
    # Bleibt /state ueber einen Neustart erhalten (Rootfs und Zustand am selben Anschluss)?
    $S 'echo hw-test > /state/hw-marker; sync'
    reboot_vm && check "$($S cat /state/hw-marker)" hw-test "/state ueberlebt Neustart" || bad "$fw/$bus/$nic: kein Neustart"
    kill $VM_PID 2>/dev/null; wait $VM_PID 2>/dev/null; VM_PID=""
done

# SSH-Key vom Stick: wie ein User unter Windows - authorized_keys.txt mit CRLF und BOM in die
# FAT-Partition "boot" legen, starten, mit genau diesem Key einloggen
echo "== SSH-Key vom Stick (boot/authorized_keys.txt)"
rm -f $SP/disk.img $SP/vars.fd $SP/userkey $SP/userkey.pub
cp "$IMG/disk.img" $SP/disk.img
ssh-keygen -q -t ed25519 -N "" -C stick-user -f $SP/userkey
printf '\357\273\277%s\r\n' "$(cat $SP/userkey.pub)" > $SP/authorized_keys.txt
BOOTOFF=$(/sbin/sfdisk -d $SP/disk.img | awk -F'[=,]' '/name="boot"/{gsub(/ /,"",$2); print $2*512}')
"$ROOT/output/host/bin/mtype" -i "$SP/disk.img@@$BOOTOFF" ::LIESMICH.txt | grep -q authorized_keys \
    && ok "LIESMICH.txt auf dem Stick" || bad "LIESMICH.txt fehlt"
"$ROOT/output/host/bin/mcopy" -i "$SP/disk.img@@$BOOTOFF" $SP/authorized_keys.txt ::authorized_keys.txt \
    && ok "Key auf den Stick kopiert (Offset $BOOTOFF)" || bad "mcopy fehlgeschlagen"
DISK_BUS=usb start_vm
wait_ssh || bad "kein SSH (Recovery-Key)"
US="timeout 30 ssh -i $SP/userkey $SSHOPTS -o IdentitiesOnly=yes root@127.0.0.1"
[ "$($US echo drin)" = drin ] && ok "Login mit dem Key vom Stick" || bad "Login mit dem Key vom Stick abgelehnt"
$S 'grep boot-keys /state/log/audit.log' | grep -q '1 Key' && ok "Audit-Log: 1 Key vom Stick" || bad "kein Audit-Eintrag"
kill $VM_PID 2>/dev/null; wait $VM_PID 2>/dev/null; VM_PID=""
finish
