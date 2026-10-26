#!/bin/sh
# Testet die zentrale Konfiguration mit "commit confirmed" in QEMU: confirm, Frist -> zurueck,
# Neustart waehrend der Frist, statische IP (richtig und falsch -> SSH weg -> System rettet sich),
# SSH-Keys hinzufuegen/entfernen, Recovery-Key, ungueltige Eingaben, Audit-Log.
# Aufruf: tests/qemu-config.sh   (KEY=... fuer anderen SSH-Key). Dauer ca. 5–8 min.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

cfg() { $S arcadeos-config show | jfield "$1"; }
wait_pending_gone() {  # wartet, bis keine Transaktion mehr offen ist (max. $1 s)
    for i in $(seq 1 "$1"); do [ "$(cfg pending)" = false ] && return 0; sleep 1; done; return 1
}

start_vm
echo "== Start"
wait_ssh && ok "SSH" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }
echo "  config: $($S arcadeos-config show)"
check "$(cfg mode)" "" "ohne Konfiguration: Vorgabe (DHCP)"
$S 'grep -q "inet dhcp" /etc/network/interfaces' && ok "interfaces: DHCP" || bad "interfaces nicht DHCP"
check "$(cfg pending)" "false" "keine offene Aenderung"

echo "== Frist verkuerzen (selbst eine Transaktion)"
$S 'arcadeos-config set config.confirm_timeout=30' >/dev/null && ok "set" || bad "set"
check "$(cfg pending)" "true" "offen"
$S arcadeos-config confirm >/dev/null && ok "confirm" || bad "confirm"
check "$(cfg pending)" "false" "bestaetigt"

echo "== Hostname aendern: Netzwerk wird dabei nicht neu gestartet"
UD1=$($S 'pidof udhcpc')
$S 'arcadeos-config set hostname=nur-name' >/dev/null && $S true && ok "SSH bleibt bei Hostname-Aenderung" || bad "Verbindung weg bei Hostname-Aenderung"
check "$($S 'pidof udhcpc')" "$UD1" "DHCP-Client nicht neu gestartet"
$S arcadeos-config confirm >/dev/null

echo "== Hostname aendern + confirm, bleibt nach Neustart"
$S 'arcadeos-config set hostname=pi-test' >/dev/null
check "$($S hostname)" "pi-test" "Hostname sofort aktiv"
$S arcadeos-config confirm >/dev/null && ok "confirm" || bad "confirm"
reboot_vm && ok "Neustart" || bad "kein Neustart"
check "$($S hostname)" "pi-test" "Hostname nach Neustart"

echo "== Ohne confirm -> nach Frist zurueck"
$S 'arcadeos-config set hostname=vergessen' >/dev/null
check "$($S hostname)" "vergessen" "Hostname vorlaeufig"
wait_pending_gone 300 && ok "Frist abgelaufen" || bad "Transaktion nach Frist noch offen"
check "$($S hostname)" "pi-test" "Hostname zurueck"

echo "== Neustart waehrend der Frist -> zurueck"
$S 'arcadeos-config set hostname=neustart' >/dev/null
reboot_vm && ok "Neustart" || bad "kein Neustart"
check "$($S hostname)" "pi-test" "Hostname nach Neustart zurueck"
check "$(cfg pending)" "false" "keine offene Aenderung"

echo "== Statische IP (richtig): neue Verbindung geht, confirm, Neustart"
$SQ 'arcadeos-config set net.mode=static net.address=10.0.2.15/24 net.gateway=10.0.2.2 net.dns=10.0.2.3' >/dev/null 2>&1
sleep 3
wait_ssh && ok "SSH ueber neue Verbindung" || bad "kein SSH mit statischer IP"
$S 'grep -q "inet static" /etc/network/interfaces' && ok "interfaces: statisch" || bad "interfaces nicht statisch"
$S 'cat /etc/resolv.conf' | grep -q '10.0.2.3' && ok "DNS gesetzt" || bad "DNS fehlt"
$S arcadeos-config confirm >/dev/null && ok "confirm" || bad "confirm"
reboot_vm && ok "Neustart" || bad "kein Neustart"
$S 'ip -4 addr show eth0' | grep -q '10.0.2.15/24' && ok "statische IP nach Neustart" || bad "statische IP fehlt"
$S 'pidof udhcpc' >/dev/null && bad "udhcpc laeuft trotz static" || ok "kein DHCP-Client"

echo "== Statische IP (falsch): SSH bricht ab, System nimmt es selbst zurueck"
$SQ 'arcadeos-config set net.address=10.9.9.9/24 net.gateway=10.9.9.1' >/dev/null 2>&1
GONE=0; for i in $(seq 1 60); do $S true 2>/dev/null || { GONE=1; break; }; sleep 1; done
[ $GONE = 1 ] && ok "SSH weg" || bad "SSH geht noch (Test ungueltig?)"
T0=$(date +%s)
wait_ssh && ok "SSH nach $(( $(date +%s) - T0 ))s wieder da" || bad "SSH kommt nicht zurueck"
$S 'ip -4 addr show eth0' | grep -q '10.0.2.15/24' && ok "alte statische IP wiederhergestellt" || bad "IP nicht wiederhergestellt"
check "$(cfg pending)" "false" "keine offene Aenderung"

echo "== Zurueck auf DHCP"
$SQ 'arcadeos-config set net.mode=dhcp' >/dev/null 2>&1; sleep 3; wait_ssh
$S arcadeos-config confirm >/dev/null && ok "DHCP bestaetigt" || bad "confirm"
$S 'pidof udhcpc' >/dev/null && ok "DHCP-Client laeuft" || bad "kein DHCP-Client"

echo "== SSH-Keys"
ssh-keygen -q -t ed25519 -N '' -C 'test@arcadeos' -f $SP/k2
S2="timeout 60 ssh -i $SP/k2 $SSHOPTS root@127.0.0.1"
$S2 true 2>/dev/null && bad "unbekannter Key kommt rein" || ok "neuer Key vorher abgelehnt"
$S "arcadeos-config add-key '$(cat $SP/k2.pub)'" >/dev/null && ok "add-key" || bad "add-key"
$S2 true 2>/dev/null && ok "neuer Key kommt rein" || bad "neuer Key abgelehnt"
$S2 arcadeos-config confirm >/dev/null && ok "confirm mit neuem Key" || bad "confirm"
$S "arcadeos-config remove-key test@arcadeos" >/dev/null && ok "remove-key" || bad "remove-key"
$S arcadeos-config confirm >/dev/null
$S2 true 2>/dev/null && bad "entfernter Key kommt noch rein" || ok "entfernter Key abgelehnt"
$S true && ok "Recovery-Key aus dem Image geht immer" || bad "Recovery-Key geht nicht"

echo "== Ungueltige Eingaben"
for bad_in in "hostname=bad_name!" "net.mode=wlan" "net.address=10.0.0.1" "net.address=10.0.0.1/33" "net.dns=1.2.3" "foo=bar"; do
    $S "arcadeos-config set '$bad_in'" >/dev/null 2>$SP/err && bad "angenommen: $bad_in" || ok "abgelehnt: $bad_in ($(cut -c1-50 $SP/err))"
done
$S "arcadeos-config add-key 'kein key'" >/dev/null 2>&1 && bad "kaputter Key angenommen" || ok "kaputter Key abgelehnt"
check "$(cfg pending)" "false" "nichts offen nach Fehlern"
check "$($S hostname)" "pi-test" "Hostname unveraendert"

echo "== Audit-Log"
A=$($S 'cat /state/log/audit.log')
for act in config-change config-confirm config-rollback; do
    echo "$A" | grep -q "\"action\":\"$act\"" && ok "Audit: $act" || bad "Audit fehlt: $act"
done
echo "$A" | grep -q 'Neustart waehrend der Frist' && ok "Audit: Rollback nach Neustart vermerkt" || bad "Audit: Neustart-Rollback fehlt"
echo "$A" | grep -q '"detail":"--timeout"' && ok "Audit: Rollback nach Frist vermerkt" || bad "Audit: Frist-Rollback fehlt"

finish
