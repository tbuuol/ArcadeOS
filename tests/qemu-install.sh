#!/bin/sh
# Testet arcadeos-install: vom USB-Stick auf eine leere NVMe-Platte installieren, dann
#   1. nur die Platte (UEFI): startet, gleicher Host-Key, Konfiguration uebernommen, state = Rest der Platte
#   2. Platte + Stick, Platte zuerst: System und /state kommen von der Platte, nicht vom Stick
#   3. Stick + Platte, Stick zuerst: der Stick bleibt Stick (eigener Zustand, Installer-Hinweis)
#   4. nur die Platte mit BIOS (SeaBIOS)
# Aufruf: tests/qemu-install.sh   (KEY=... fuer anderen SSH-Key). Dauer ca. 15 min.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

cp "$IMG/disk.img" $SP/stick.img
truncate -s 6G $SP/target.img
# Installation: Kopieren und Byte-Vergleich von ~1 GB dauern in der emulierten VM einige Minuten
SL=$(echo "$S" | sed 's/^timeout 60 /timeout 1500 /')
STICK="-device qemu-xhci,id=xhci2 -drive file=$SP/stick.img,if=none,id=st,format=raw -device usb-storage,bus=xhci2.0,drive=st"
TARGET="-drive file=$SP/target.img,if=none,id=tg,format=raw -device nvme,drive=tg,serial=target1"

# VM starten: run.sh-Umgebung + zusaetzliche QEMU-Argumente
boot() {  # <name> <umgebung...> -- <qemu-argumente...>
    name=$1; shift
    env_args=""
    while [ "$1" != -- ]; do env_args="$env_args $1"; shift; done; shift
    env DEBUG_DIR=$SP/debug VARS=$SP/vars.fd SSH_PORT=2222 $env_args "$ROOT/run.sh" \
        -serial file:$SP/serial-$name.log -monitor none -display none "$@" >/dev/null 2>&1 &
    VM_PID=$!
}
stop() { kill "$VM_PID" 2>/dev/null; wait "$VM_PID" 2>/dev/null; VM_PID=""; }

echo "== Vom Stick starten (USB), leere NVMe-Platte daneben"
boot stick DISK=$SP/stick.img DISK_BUS=usb -- $TARGET
wait_ssh && ok "SSH vom Stick" || { bad "kein SSH"; tail -30 $SP/serial-stick.log; exit 1; }
check "$($S '. /usr/lib/arcadeos/common.sh; boot_disk')" "sda" "Startplatte = Stick"
R=$($S cat /proc/cmdline | grep -o 'root=[^ ]*')
case "$R" in root=PARTUUID=*) ok "GRUB startet das System per PARTUUID ($R)" ;; *) bad "kein PARTUUID: $R" ;; esac
$S '/usr/lib/arcadeos/status-screen --once' | grep -q '^Stick:  zum Installieren' && ok "Statusseite: Stick-Hinweis" || bad "Statusseite ohne Stick-Hinweis"

L=$($S 'arcadeos-install list')
echo "$L" > $SP/list.json
check "$(echo "$L" | python3 -I -c 'import json,sys; d=json.load(sys.stdin); print(d["boot_disk"], [(x["name"],x["bus"],x["size_gb"]) for x in d["disks"]])')" \
    "sda [('nvme0n1', 'nvme', 6)]" "list: nur die NVMe-Platte, nicht der Stick"
$S 'arcadeos-install to sda --wipe sda' >/dev/null 2>&1 && bad "Installation auf den Stick selbst angenommen" || ok "Startplatte als Ziel abgelehnt"
$S 'arcadeos-install to nvme0n1 --wipe sda' >/dev/null 2>&1 && bad "falsche Bestaetigung angenommen" || ok "falsche Bestaetigung abgelehnt"
$S 'arcadeos-install to nvme0n1' >/dev/null 2>&1 && bad "ohne --wipe angenommen" || ok "ohne --wipe abgelehnt"
$S 'arcadeos-install to gibtsnicht --wipe gibtsnicht' >/dev/null 2>&1 && bad "unbekannte Platte angenommen" || ok "unbekannte Platte abgelehnt"
$S 'head -c 4096 /dev/nvme0n1 | tr -d "\0" | wc -c' | grep -qx 0 && ok "Ablehnungen haben nichts geschrieben" || bad "Platte wurde beschrieben"

# Zustand, der mitkommen soll
$S 'arcadeos-config set hostname=inst-test' >/dev/null && $S 'arcadeos-config confirm' >/dev/null
FP=$($S 'dropbearkey -y -f /etc/dropbear/dropbear_ed25519_host_key' | sed -n 's/^Fingerprint: *//p')
NODE=$($S 'cat /run/meshd.json' | jfield node_id)
SLOTA=$($S 'cat /state/update/slot-A.manifest' | sed -n 's/^version=//p')
$S 'mkdir -p /state/update/incoming && echo halb > /state/update/incoming/x'

T0=$(date +%s)
$SL 'arcadeos-install to nvme0n1 --wipe nvme0n1' > $SP/install.log 2>&1
RC=$?
echo "  (Installation: $(( $(date +%s) - T0 )) s)"
[ $RC = 0 ] && tail -1 $SP/install.log | grep -q '"pct":100' && ok "Installation durchgelaufen" || { bad "Installation: $(tail -5 $SP/install.log)"; }
grep -c '"step":"System kopieren' $SP/install.log | grep -qv '^0$' && ok "Fortschritt in % ($(grep -c pct $SP/install.log) Meldungen)" || bad "kein Fortschritt"
$S 'tail -1 /state/log/audit.log' | grep -q '"action":"install-done"' && ok "Audit: install-done" || bad "Audit: $($S 'tail -2 /state/log/audit.log')"
P=$($S 'sgdisk -p /dev/nvme0n1' | awk '/^ +[0-9]+ /{print $1":"$NF}' | tr '\n' ' ')
check "$P" "1:bios-boot 2:boot 3:rootfs-a 4:rootfs-b 5:arcadeos 6:state " "Partitionen auf der Platte"
check "$($S 'sgdisk -v /dev/nvme0n1' | grep -c 'No problems found')" "1" "GPT der Platte fehlerfrei"
U1=$($S 'sgdisk -i 3 /dev/sda' | sed -n 's/^Partition unique GUID: //p')
U2=$($S 'sgdisk -i 3 /dev/nvme0n1' | sed -n 's/^Partition unique GUID: //p')
[ -n "$U1" ] && [ -n "$U2" ] && [ "$U1" != "$U2" ] && ok "eigene Partitions-IDs (nicht die des Sticks)" || bad "Partitions-IDs: $U1 / $U2"
$SL 'arcadeos-install to nvme0n1 --wipe nvme0n1' >/dev/null 2>&1 && ok "zweite Installation auf dieselbe Platte (ueberschreibt)" || bad "zweite Installation fehlgeschlagen"
E=$($S 'efibootmgr')
check "$(echo "$E" | grep -c ' ArcadeOS')" "1" "genau ein UEFI-Starteintrag ArcadeOS (auch nach zweiter Installation)"
FIRST=$(echo "$E" | sed -n 's/^BootOrder: \([0-9A-F]*\).*/\1/p')
echo "$E" | grep -q "^Boot$FIRST\*\{0,1\} ArcadeOS" && ok "ArcadeOS steht vorn in der Startreihenfolge" || bad "Startreihenfolge: $E"
stop

echo "== 1. Nur die Platte (UEFI)"
boot disk DISK=$SP/target.img DISK_BUS=nvme --
wait_ssh && ok "SSH von der Platte" || { bad "Platte startet nicht"; tail -30 $SP/serial-disk.log; exit 1; }
check "$($S '. /usr/lib/arcadeos/common.sh; boot_disk')" "nvme0n1" "Startplatte = NVMe"
check "$($S hostname)" "inst-test" "Konfiguration uebernommen"
check "$($S 'dropbearkey -y -f /etc/dropbear/dropbear_ed25519_host_key' | sed -n 's/^Fingerprint: *//p')" "$FP" "gleicher Host-Key"
for i in $(seq 1 30); do $S 'test -f /run/meshd.json' && break; sleep 1; done
check "$($S 'cat /run/meshd.json' | jfield node_id)" "$NODE" "gleiche Mesh-Identitaet"
check "$($S 'cat /run/state.json' | jfield device)" "/dev/nvme0n1p6" "/state auf der Platte"
SZ=$($S 'df -k /state' | awk 'NR==2{print int($2/1048576)}')
[ "$SZ" -ge 4 ] && ok "state nutzt den Rest der Platte ($SZ GB)" || bad "state nur $SZ GB"
check "$($S 'grep " /state " /proc/mounts' | awk '{print $3}')" "ext4" "state ist ext4"
$S 'dumpe2fs -h /dev/nvme0n1p6 2>/dev/null' | grep -q 'has_journal' && ok "state mit Journal" || bad "state ohne Journal"
$S 'test -f /state/installed.json' && ok "installed.json" || bad "installed.json fehlt"
$S '/usr/lib/arcadeos/status-screen --once' | grep -q '^Platte: /dev/nvme0n1 (installiert)' && ok "Statusseite: installiert" || bad "Statusseite: $($S '/usr/lib/arcadeos/status-screen --once' | grep -E '^(Stick|Platte)')"
$S 'test -e /state/update/incoming' && bad "halbes Update mitkopiert" || ok "kein halbes Update mitkopiert"
check "$($S 'cat /state/update/slot-A.manifest' | sed -n 's/^version=//p')" "$SLOTA" "Versionsangabe von Slot A uebernommen"
$S 'arcadeos-install list' | grep -q '"disks":\[\]' && ok "list auf der Platte: keine weiteren Platten" || bad "list: $($S 'arcadeos-install list')"
stop

echo "== 2. Platte + Stick, Platte zuerst"
boot both1 DISK=$SP/target.img DISK_BUS=nvme BOOTINDEX=0 -- $STICK
wait_ssh && ok "SSH" || { bad "startet nicht"; tail -30 $SP/serial-both1.log; exit 1; }
check "$($S '. /usr/lib/arcadeos/common.sh; boot_disk')" "nvme0n1" "System von der Platte"
check "$($S 'cat /run/state.json' | jfield device)" "/dev/nvme0n1p6" "/state von der Platte, nicht vom Stick"
check "$($S hostname)" "inst-test" "Konfiguration der Platte"
stop

echo "== 3. Stick + Platte, Stick zuerst"
boot both2 DISK=$SP/stick.img DISK_BUS=usb BOOTINDEX=0 -- $TARGET
wait_ssh && ok "SSH" || { bad "startet nicht"; tail -30 $SP/serial-both2.log; exit 1; }
check "$($S '. /usr/lib/arcadeos/common.sh; boot_disk')" "sda" "System vom Stick"
check "$($S 'cat /run/state.json' | jfield device)" "/dev/sda6" "/state vom Stick, nicht von der Platte"
$S 'test -f /state/installed.json' && bad "Stick haelt sich fuer installiert" || ok "Stick bleibt Stick"
stop

echo "== 4. Nur die Platte, BIOS"
boot bios DISK=$SP/target.img DISK_BUS=nvme FIRMWARE=bios --
wait_ssh && ok "SSH (BIOS-Start von der Platte)" || { bad "BIOS-Start fehlgeschlagen"; tail -30 $SP/serial-bios.log; }
check "$($S hostname)" "inst-test" "gleiches System"
stop

finish
