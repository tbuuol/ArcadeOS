# Gemeinsame Hilfsfunktionen fuer die QEMU-Tests (wird per "." eingebunden).
# Erwartet: $ROOT (Repo), optional $1 des Aufrufers als SSH-Key.

SP=$(mktemp -d)
VM_PID=""
# nur die eigene VM beenden (eine dauerhaft laufende Live-VM bleibt unberuehrt)
# Bei Fehlschlag: Logs (seriell, QEMU, Firmware, Haenger-Abfragen) nach test-logs/ sichern
keep_logs() {
    [ "$FAIL" -gt 0 ] 2>/dev/null || return 0
    D="$ROOT/test-logs/$(basename "$0" .sh)-$(date +%Y%m%d-%H%M%S)"
    mkdir -p "$D" && cp -r "$SP"/*.log "$SP"/debug "$D"/ 2>/dev/null
    echo "  Logs gesichert: $D"
}
trap 'keep_logs; [ -n "$VM_PID" ] && kill "$VM_PID" 2>/dev/null; rm -rf "$SP"' EXIT
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
    DEBUG_DIR=$SP/debug DISK=$SP/disk.img VARS=$SP/vars.fd SSH_PORT=2222 "$ROOT/run.sh" -serial file:$SP/serial.log -monitor none -display none "$@" >/dev/null 2>&1 &
    VM_PID=$!   # run.sh ersetzt sich per exec durch qemu: gleiche PID
}
bootid() { $S cat /proc/sys/kernel/random/boot_id 2>/dev/null; }
# Grosszuegige Zeitlimits: ohne KVM wird die Emulation auf einem geteilten Host zeitweise 10x langsamer
# (gemessen: Neustart bis 6 min). Langsam ist kein Fehler, nicht wiederkommen schon.
# Zeitgrenze nach Uhr, nicht nach Versuchen (ein Versuch kann bis zu ConnectTimeout dauern)
wait_ssh() {
    T_END=$(( $(date +%s) + ${1:-600} ))
    while [ "$(date +%s)" -lt $T_END ]; do $S true 2>/dev/null && return 0; sleep 1; done
    hang_dump "wait_ssh"; return 1
}
# wartet, bis das System mit einer neuen boot_id wieder per SSH erreichbar ist
wait_reboot() {
    T0=$(date +%s)
    while [ $(( $(date +%s) - T0 )) -lt "${2:-600}" ]; do
        b=$(bootid); [ -n "$b" ] && [ "$b" != "$1" ] && { WAITED=$(( $(date +%s) - T0 )); return 0; }
        sleep 1
    done
    hang_dump "wait_reboot"
    return 1
}
# Kommt die VM nicht zurueck: solange sie noch haengt, ueber QMP abfragen (Register, Befehlszeiger)
hang_dump() {
    [ -S "$SP/debug/qmp.sock" ] || return 0
    echo "  VM antwortet nicht ($1) – Abfrage ueber QMP nach debug/hang-$1.txt"
    python3 -I "$ROOT/tools/qmp.py" "$SP/debug/qmp.sock" dump 10 > "$SP/debug/hang-$1.txt" 2>&1
}
# Neustart; kommt die VM nach 60 s noch mit derselben Boot-ID, war der Befehl nicht angekommen: wiederholen
reboot_vm() {
    b=$(bootid); $S reboot >/dev/null 2>&1
    for i in $(seq 1 60); do sleep 1; n=$(bootid); [ -n "$n" ] && [ "$n" != "$b" ] && { WAITED=$i; return 0; }; done
    [ "$(bootid)" = "$b" ] && { echo "  (reboot nicht angekommen, erneut)"; $S reboot >/dev/null 2>&1; }
    wait_reboot "$b"
}
# Wert eines (eindeutigen) Schluessels aus einer JSON-Zeile
jfield() { sed -n "s/.*\"$1\":\"\{0,1\}\([^\",}]*\).*/\1/p"; }
check() {  # wert erwartet beschreibung
    [ "$1" = "$2" ] && ok "$3 ($1)" || bad "$3: '$1', erwartet '$2'"
}
finish() {
    echo "== Ergebnis: $PASS ok, $FAIL fehlgeschlagen"
    [ $FAIL = 0 ]
}
