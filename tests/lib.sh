# Gemeinsame Hilfsfunktionen fuer die QEMU-Tests (wird per "." eingebunden).
# Erwartet: $ROOT (Repo), optional $1 des Aufrufers als SSH-Key.

SP=$(mktemp -d)
trap 'pkill -x qemu-system-x86; rm -rf "$SP"' EXIT
IMG="$ROOT/output/images"
KEY=${KEY:-$HOME/.ssh/aios_ed25519}
SSHOPTS="-p 2222 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=3 -o ServerAliveInterval=5 -o ServerAliveCountMax=6 -o BatchMode=yes"
# timeout + Keepalive: bricht die VM mitten in einer Verbindung weg, haengt ssh sonst minutenlang
S="timeout 60 ssh -i $KEY $SSHOPTS root@127.0.0.1"
# fuer Befehle, die absichtlich das Netzwerk umwerfen: nicht an der abgerissenen Verbindung haengen
SQ="timeout 10 ssh -i $KEY $SSHOPTS root@127.0.0.1"
MINISIGN="$ROOT/output/host/bin/minisign"
PASS=0; FAIL=0

ok()  { echo "  OK   $*"; PASS=$((PASS+1)); }
bad() { echo "  FAIL $*"; FAIL=$((FAIL+1)); }

start_vm() {  # Kopie von disk.img in $SP
    [ -f $SP/disk.img ] || cp "$IMG/disk.img" $SP/disk.img
    DISK=$SP/disk.img VARS=$SP/vars.fd "$ROOT/run.sh" -serial file:$SP/serial.log -monitor none -display none >/dev/null 2>&1 &
}
bootid() { $S cat /proc/sys/kernel/random/boot_id 2>/dev/null; }
# Grosszuegige Zeitlimits: ohne KVM wird die Emulation auf einem geteilten Host zeitweise 10x langsamer
# (gemessen: Neustart bis 6 min). Langsam ist kein Fehler, nicht wiederkommen schon.
wait_ssh() { for i in $(seq 1 600); do $S true 2>/dev/null && return 0; sleep 1; done; return 1; }
# wartet, bis das System mit einer neuen boot_id wieder per SSH erreichbar ist
wait_reboot() {
    T0=$(date +%s)
    for i in $(seq 1 "${2:-600}"); do
        b=$(bootid); [ -n "$b" ] && [ "$b" != "$1" ] && { WAITED=$(( $(date +%s) - T0 )); return 0; }
        sleep 1
    done
    return 1
}
reboot_vm() { b=$(bootid); $S reboot >/dev/null 2>&1; wait_reboot "$b"; }
# Wert eines (eindeutigen) Schluessels aus einer JSON-Zeile
jfield() { sed -n "s/.*\"$1\":\"\{0,1\}\([^\",}]*\).*/\1/p"; }
check() {  # wert erwartet beschreibung
    [ "$1" = "$2" ] && ok "$3 ($1)" || bad "$3: '$1', erwartet '$2'"
}
finish() {
    echo "== Ergebnis: $PASS ok, $FAIL fehlgeschlagen"
    [ $FAIL = 0 ]
}
