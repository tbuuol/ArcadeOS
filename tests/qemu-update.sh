#!/bin/sh
# Testet A/B-Updates in QEMU: kaputte Pakete, Install per SSH-Stream, Probeboot ohne Bestaetigung
# (Frist -> zurueck), confirm, Watchdog bei haengendem Probeboot, rollback.
# Aufruf: tests/qemu-update.sh [ssh-private-key]   (Standard: ~/.ssh/aios_ed25519)
# Braucht Port 2222 frei; stoppt am Ende alle qemu-system-x86-Prozesse des Users. Dauer ca. 6 min.
SP=$(mktemp -d)
trap 'pkill -x qemu-system-x86; rm -rf "$SP"' EXIT
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMG="$ROOT/output/images"
KEY=${1:-$HOME/.ssh/aios_ed25519}
# timeout + Keepalive: bricht die VM mitten in einer Verbindung weg, haengt ssh sonst minutenlang
S="timeout 60 ssh -p 2222 -i $KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=3 -o ServerAliveInterval=2 -o ServerAliveCountMax=3 -o BatchMode=yes root@127.0.0.1"
PASS=0; FAIL=0
ok()  { echo "  OK   $*"; PASS=$((PASS+1)); }
bad() { echo "  FAIL $*"; FAIL=$((FAIL+1)); }

start_vm() {
    DISK=$SP/disk.img VARS=$SP/vars.fd "$ROOT/run.sh" -serial file:$SP/serial.log -monitor none -display none >/dev/null 2>&1 &
}
bootid() { $S cat /proc/sys/kernel/random/boot_id 2>/dev/null; }
wait_ssh() { for i in $(seq 1 240); do $S true 2>/dev/null && return 0; sleep 1; done; return 1; }
# wartet, bis das System mit einer neuen boot_id wieder per SSH erreichbar ist
wait_reboot() {
    T0=$(date +%s)
    for i in $(seq 1 "${2:-300}"); do
        b=$(bootid); [ -n "$b" ] && [ "$b" != "$1" ] && { WAITED=$(( $(date +%s) - T0 )); return 0; }
        sleep 1
    done
    return 1
}
reboot_vm() { b=$(bootid); $S reboot >/dev/null 2>&1; wait_reboot "$b"; }
field() { $S arcadeos-update status | sed -n "s/.*\"$1\":\"\{0,1\}\([^\",}]*\).*/\1/p"; }
expect() {  # feld wert beschreibung
    v=$(field "$1"); [ "$v" = "$2" ] && ok "$3 ($1=$v)" || bad "$3: $1='$v', erwartet '$2'"
}

cp "$IMG/disk.img" $SP/disk.img
start_vm
echo "== Start"
wait_ssh && ok "SSH" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }
echo "  status: $($S arcadeos-update status)"
expect running A "laeuft auf Slot A"
expect fixed A "fester Slot A"

echo "== Kaputte Pakete werden abgelehnt"
mkdir $SP/pkg && tar -x -C $SP/pkg -f "$IMG/update.tar"
cp -r $SP/pkg $SP/badsum && sed -i 's/^rootfs_sha256=.*/rootfs_sha256=00/' $SP/badsum/manifest
tar -c -f $SP/badsum.tar -C $SP/badsum manifest rootfs.squashfs bzImage
$S 'arcadeos-update install -' < $SP/badsum.tar 2>$SP/err && bad "falsche Pruefsumme angenommen" || ok "falsche Pruefsumme: $(cat $SP/err)"
cp -r $SP/pkg $SP/badarch && sed -i 's/^arch=.*/arch=aarch64/' $SP/badarch/manifest
tar -c -f $SP/badarch.tar -C $SP/badarch manifest rootfs.squashfs bzImage
$S 'arcadeos-update install -' < $SP/badarch.tar 2>$SP/err && bad "falsche Architektur angenommen" || ok "falsche Architektur: $(cat $SP/err)"
expect try_slot "" "kein Probeboot gesetzt"

echo "== Install per SSH-Stream, Probeboot ohne Bestaetigung -> Frist -> zurueck"
$S 'echo 45 > /state/update/confirm_timeout'
$S 'arcadeos-update install -' < "$IMG/update.tar" && ok "install" || bad "install"
expect try_slot B "Probeboot auf B gesetzt"
reboot_vm && ok "Neustart (${WAITED}s)" || bad "kein Neustart"
expect running B "laeuft im Probeboot auf B"
expect trying true "als Probeboot erkannt"
echo "  status: $($S arcadeos-update status)"
$S 'arcadeos-update install -' < "$IMG/update.tar" 2>$SP/err && bad "Install im Probeboot erlaubt" || ok "Install im Probeboot abgelehnt"
b=$(bootid)
wait_reboot "$b" 200 && ok "ohne confirm nach Frist neu gestartet (${WAITED}s)" || bad "kein Neustart nach Frist"
expect running A "zurueck auf Slot A"
expect fixed A "fester Slot bleibt A"
expect try_slot "" "Probeboot verbraucht"

echo "== Install, Probeboot, confirm"
$S 'echo 600 > /state/update/confirm_timeout'
$S 'arcadeos-update install -' < "$IMG/update.tar" >/dev/null && ok "install" || bad "install"
reboot_vm && ok "Neustart (${WAITED}s)" || bad "kein Neustart"
expect running B "Probeboot auf B"
$S arcadeos-update confirm >/dev/null && ok "confirm" || bad "confirm"
expect fixed B "B ist fest"
expect trying false "kein Probeboot mehr"
reboot_vm && ok "Neustart (${WAITED}s)" || bad "kein Neustart"
expect running B "bleibt nach Neustart auf B"
ST=$($S arcadeos-update status); echo "  status: $ST"
echo "$ST" | grep -q '"A":{"version":""}' && bad "Version von Slot A unbekannt" || ok "Version beider Slots bekannt"

echo "== Watchdog: haengender Probeboot -> zurueck"
$S 'arcadeos-update install -' < "$IMG/update.tar" >/dev/null && ok "install nach A" || bad "install nach A"
reboot_vm && ok "Neustart (${WAITED}s)" || bad "kein Neustart"
expect running A "Probeboot auf A"
$S 'pidof watchdog' >/dev/null && ok "Watchdog-Dienst laeuft" || bad "Watchdog-Dienst laeuft nicht"
b=$(bootid)
$S 'killall -9 watchdog'
wait_reboot "$b" 200 && ok "Watchdog hat neu gestartet (${WAITED}s nach Haenger)" || bad "kein Watchdog-Neustart"
expect running B "zurueck auf festem Slot B"

echo "== Rollback"
$S arcadeos-update rollback >/dev/null && ok "rollback" || bad "rollback"
expect fixed A "A wieder fest"
reboot_vm && ok "Neustart (${WAITED}s)" || bad "kein Neustart"
expect running A "laeuft auf A"

echo "== Ergebnis: $PASS ok, $FAIL fehlgeschlagen"
[ $FAIL = 0 ]
