#!/bin/sh
# Testet A/B-Updates in QEMU: Signaturen (fehlend, fremder Key, verfaelschtes Manifest), kaputte
# Pakete, Install per SSH-Stream, Probeboot ohne Bestaetigung (Frist -> zurueck), confirm,
# Watchdog bei haengendem Probeboot, rollback, Audit-Log.
# Aufruf: tests/qemu-update.sh   (KEY=... fuer anderen SSH-Key)
# Braucht Port 2222 frei; stoppt am Ende alle qemu-system-x86-Prozesse des Users. Dauer ca. 6–10 min.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

field() { $S arcadeos-update status | jfield "$1"; }
expect() { check "$(field "$1")" "$2" "$3"; }
# Paket aus Verzeichnis bauen (mit allen Signaturen, die darin liegen)
mkpkg() { (cd "$1" && tar -c -f "$2" manifest $(ls manifest.*.minisig 2>/dev/null) rootfs.squashfs bzImage); }
resign_dev() { rm -f "$1"/manifest.*.minisig; "$MINISIGN" -S -s "$HOME/.config/arcadeos/dev.key" -m "$1/manifest" -x "$1/manifest.dev.minisig" >/dev/null; }

start_vm
echo "== Start"
wait_ssh && ok "SSH" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }
echo "  status: $($S arcadeos-update status)"
expect running A "laeuft auf Slot A"
expect fixed A "fester Slot A"
check "$($S 'ls /etc/arcadeos/update-keys' | tr '\n' ' ')" "dev-build-vmi.pub " "Dev-Build vertraut dem Build-Key"

echo "== Signaturen"
mkdir $SP/pkg && tar -x -C $SP/pkg -f "$IMG/update.tar"
[ -f $SP/pkg/manifest.dev.minisig ] && ok "Paket aus dem Build ist signiert" || bad "Paket ohne Signatur"
cp -r $SP/pkg $SP/nosig && rm -f $SP/nosig/manifest.*.minisig && mkpkg $SP/nosig $SP/nosig.tar
$S 'arcadeos-update install -' < $SP/nosig.tar 2>$SP/err && bad "unsigniertes Paket angenommen" || ok "unsigniert: $(cut -c1-70 $SP/err)"
"$MINISIGN" -G -W -p $SP/fremd.pub -s $SP/fremd.key >/dev/null
cp -r $SP/nosig $SP/fremd && mkpkg $SP/fremd $SP/fremd.tar
PATH="$ROOT/output/host/bin:$PATH" "$ROOT/tools/sign-update.sh" $SP/fremd.tar $SP/fremd.key >/dev/null 2>&1 \
    && tar -tf $SP/fremd.tar | grep -qx 'manifest.fremd.minisig' && ok "tools/sign-update.sh signiert mit eigenem Key" || bad "sign-update.sh"
$S 'arcadeos-update install -' < $SP/fremd.tar 2>$SP/err && bad "Paket mit fremdem Key angenommen" || ok "fremder Key: abgelehnt"
cp -r $SP/pkg $SP/forged && sed -i 's/^version=.*/version=gefaelscht/' $SP/forged/manifest && mkpkg $SP/forged $SP/forged.tar
$S 'arcadeos-update install -' < $SP/forged.tar 2>$SP/err && bad "verfaelschtes Manifest angenommen" || ok "Manifest nach dem Signieren geaendert: abgelehnt"
expect try_slot "" "kein Probeboot gesetzt"

echo "== Kaputte, aber gueltig signierte Pakete werden abgelehnt"
cp -r $SP/pkg $SP/badsum && sed -i 's/^rootfs_sha256=.*/rootfs_sha256=00/' $SP/badsum/manifest && resign_dev $SP/badsum && mkpkg $SP/badsum $SP/badsum.tar
$S 'arcadeos-update install -' < $SP/badsum.tar 2>$SP/err && bad "falsche Pruefsumme angenommen" || ok "falsche Pruefsumme: $(cat $SP/err)"
cp -r $SP/pkg $SP/badarch && sed -i 's/^arch=.*/arch=aarch64/' $SP/badarch/manifest && resign_dev $SP/badarch && mkpkg $SP/badarch $SP/badarch.tar
$S 'arcadeos-update install -' < $SP/badarch.tar 2>$SP/err && bad "falsche Architektur angenommen" || ok "falsche Architektur: $(cat $SP/err)"
expect try_slot "" "kein Probeboot gesetzt"

echo "== Install per SSH-Stream, Probeboot ohne Bestaetigung -> Frist -> zurueck"
$S 'echo 45 > /state/update/confirm_timeout'
OUT=$($S 'arcadeos-update install -' < "$IMG/update.tar" 2>/dev/null) && ok "install: $OUT" || bad "install"
check "$(echo "$OUT" | jfield signer)" "dev-build-vmi" "Signatur vom Build-Key erkannt"
expect try_slot B "Probeboot auf B gesetzt"
reboot_vm && ok "Neustart" || bad "kein Neustart"
expect running B "laeuft im Probeboot auf B"
expect trying true "als Probeboot erkannt"
$S 'arcadeos-update install -' < "$IMG/update.tar" 2>/dev/null && bad "Install im Probeboot erlaubt" || ok "Install im Probeboot abgelehnt"
b=$(bootid)
wait_reboot "$b" 600 && ok "ohne confirm nach Frist neu gestartet" || bad "kein Neustart nach Frist"
expect running A "zurueck auf Slot A"
expect fixed A "fester Slot bleibt A"
expect try_slot "" "Probeboot verbraucht"

echo "== Install, Probeboot, confirm"
$S 'echo 600 > /state/update/confirm_timeout'
$S 'arcadeos-update install -' < "$IMG/update.tar" >/dev/null 2>&1 && ok "install" || bad "install"
reboot_vm && ok "Neustart" || bad "kein Neustart"
expect running B "Probeboot auf B"
$S arcadeos-update confirm >/dev/null && ok "confirm" || bad "confirm"
expect fixed B "B ist fest"
expect trying false "kein Probeboot mehr"
reboot_vm && ok "Neustart" || bad "kein Neustart"
expect running B "bleibt nach Neustart auf B"
ST=$($S arcadeos-update status); echo "  status: $ST"
echo "$ST" | grep -q '"A":{"version":""}' && bad "Version von Slot A unbekannt" || ok "Version beider Slots bekannt"

echo "== Watchdog: haengender Probeboot -> zurueck"
$S 'arcadeos-update install -' < "$IMG/update.tar" >/dev/null 2>&1 && ok "install nach A" || bad "install nach A"
reboot_vm && ok "Neustart" || bad "kein Neustart"
expect running A "Probeboot auf A"
$S 'pidof watchdog' >/dev/null && ok "Watchdog-Dienst laeuft" || bad "Watchdog-Dienst laeuft nicht"
b=$(bootid)
$S 'killall -9 watchdog'
wait_reboot "$b" 600 && ok "Watchdog hat neu gestartet" || bad "kein Watchdog-Neustart"
expect running B "zurueck auf festem Slot B"

echo "== Rollback"
$S arcadeos-update rollback >/dev/null && ok "rollback" || bad "rollback"
expect fixed A "A wieder fest"
reboot_vm && ok "Neustart" || bad "kein Neustart"
expect running A "laeuft auf A"

echo "== Audit-Log"
A=$($S 'cat /state/log/audit.log')
for act in update-rejected update-install update-timeout update-confirm update-rollback; do
    echo "$A" | grep -q "\"action\":\"$act\"" && ok "Audit: $act" || bad "Audit fehlt: $act"
done
echo "$A" | tail -2 | sed 's/^/  /'

finish
