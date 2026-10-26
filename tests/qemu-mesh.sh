#!/bin/sh
# Testet meshd im Image (QEMU, braucht Internet): Verbindungen zu Live-Arcades, Mesh-Key bleibt ueber
# Neustart gleich, Debug-Log, agentd mesh_status, Neustart nach Absturz, sauberes Beenden.
# Aufruf: tests/qemu-mesh.sh   (KEY=... fuer anderen SSH-Key). Dauer ca. 3–6 min.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

links() { $S 'meshd status' 2>/dev/null | python3 -I -c "import json,sys; print(len(json.load(sys.stdin).get('links',[])))" 2>/dev/null || echo 0; }
wait_links() {  # mindestens $1 Verbindungen, hoechstens $2 s warten
    for i in $(seq 1 "$2"); do [ "$(links)" -ge "$1" ] && return 0; sleep 1; done; return 1
}
nodeid() { $S 'meshd status' | python3 -I -c "import json,sys; print(json.load(sys.stdin)['node_id'])"; }

start_vm
echo "== Start"
wait_ssh && ok "SSH" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }
$S 'pidof meshd' >/dev/null && ok "meshd laeuft" || bad "meshd laeuft nicht"

echo "== Verbindungen zu Live-Arcades"
wait_links 2 180 && ok "$(links) Verbindungen" || bad "weniger als 2 Verbindungen: $($S 'meshd status')"
$S 'meshd status' | python3 -I -c "
import json,sys
d=json.load(sys.stdin)
for l in d['links']: print('       ', l['addr'], l['transport'], 'rtt', l['rtt_ms'], 'ms')"
ID1=$(nodeid); echo "  Knoten-ID: $ID1"
$S 'ls -l /state/mesh/key' | grep -q -- '-rw-------' && ok "Mesh-Key auf /state, nur root lesbar" || bad "Mesh-Key fehlt oder falsche Rechte"

echo "== Debug-Log"
$S 'grep -c "\"ev\":\"link_up\"" /state/mesh/debug.log' | grep -qv '^0$' && ok "link_up im Debug-Log" || bad "kein link_up im Debug-Log"
$S 'head -1 /state/mesh/debug.log' | python3 -I -c "import json,sys; d=json.loads(sys.stdin.read()); assert d['ev'] in ('key_created','start')" 2>/dev/null \
    && ok "Debug-Log ist JSON" || bad "Debug-Log nicht lesbar"

echo "== agentd mesh_status"
R=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"mesh_status","arguments":{}}}' | $S agentd)
N=$(printf %s "$R" | python3 -I -c "import json,sys; r=json.load(sys.stdin); print(len(json.loads(r['result']['content'][0]['text'])['links']))" 2>/dev/null)
[ "${N:-0}" -ge 1 ] && ok "mesh_status: $N Verbindungen" || bad "mesh_status: $R"

echo "== Absturz -> Neustart"
P1=$($S 'pidof meshd')
$S 'kill -9 $(pidof meshd)'
P2=""; for i in $(seq 1 30); do P2=$($S 'pidof meshd' 2>/dev/null); [ -n "$P2" ] && [ "$P2" != "$P1" ] && break; sleep 1; done
[ -n "$P2" ] && [ "$P2" != "$P1" ] && ok "meshd neu gestartet ($P1 -> $P2)" || bad "meshd nicht neu gestartet"
wait_links 1 120 && ok "wieder verbunden" || bad "nach Neustart keine Verbindung"
check "$(nodeid)" "$ID1" "gleiche Knoten-ID nach Absturz"

echo "== Neustart des Systems"
reboot_vm && ok "Neustart" || bad "kein Neustart"
wait_links 1 180 && ok "nach Neustart verbunden" || bad "nach Neustart keine Verbindung"
check "$(nodeid)" "$ID1" "gleiche Knoten-ID nach Neustart"
$S 'grep -c "\"ev\":\"shutdown\"" /state/mesh/debug.log' | grep -qv '^0$' && ok "sauber beendet (shutdown im Log)" || bad "kein sauberes Beenden im Log"
$S 'test -s /state/mesh/peers.json' && ok "Adressbuch gespeichert" || bad "kein Adressbuch"

finish
