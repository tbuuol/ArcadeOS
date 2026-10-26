#!/bin/sh
# Testet Netztuning und Zeitsynchronisation in QEMU: BBR, fq, tcp_notsent_lowat, NTP-Sync,
# ntp.servers ueber arcadeos-config, Anzeige in agentd.
# Aufruf: tests/qemu-net.sh   (KEY=... fuer anderen SSH-Key). Braucht Internet (NTP). Dauer ca. 2–4 min.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

start_vm
echo "== Start"
wait_ssh && ok "SSH" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }

echo "== Netztuning"
check "$($S cat /proc/sys/net/ipv4/tcp_congestion_control)" "bbr" "Staukontrolle"
check "$($S cat /proc/sys/net/core/default_qdisc)" "fq" "Standard-Warteschlange"
check "$($S cat /proc/sys/net/ipv4/tcp_notsent_lowat)" "16384" "tcp_notsent_lowat"
Q=$($S 'tc qdisc show dev eth0')
echo "$Q" | grep -qE 'qdisc (fq|mq) ' && ok "eth0 nutzt fq/mq: $(echo "$Q" | head -1)" || bad "eth0-Warteschlange: $Q"
$S 'tc qdisc show dev eth0' | grep -q ' fq ' && ok "fq auf eth0 aktiv" || bad "kein fq auf eth0"

echo "== Zeitsynchronisation"
$S 'pidof ntpd' >/dev/null && ok "ntpd laeuft" || bad "ntpd laeuft nicht"
SYNC=""
for i in $(seq 1 120); do SYNC=$($S 'cat /run/ntp.json' 2>/dev/null); echo "$SYNC" | grep -q '"synced":true' && break; sleep 2; done
echo "$SYNC" | grep -q '"synced":true' && ok "synchronisiert: $SYNC" || bad "nicht synchronisiert: $SYNC"
HOST_T=$(date -u +%s); VM_T=$($S 'date -u +%s'); D=$(( HOST_T - VM_T )); [ ${D#-} -le 5 ] && ok "Uhr weicht ${D#-} s vom Host ab" || bad "Uhr weicht $D s ab"

echo "== ntp.servers ueber arcadeos-config"
$S 'arcadeos-config set ntp.servers="time.cloudflare.com pool.ntp.org"' >/dev/null && ok "set" || bad "set"
sleep 2
$S 'ps' | grep -q 'ntpd.*time.cloudflare.com' && ok "ntpd mit neuem Server neu gestartet" || bad "ntpd nicht umgestellt: $($S 'ps | grep ntp[d]')"
$S arcadeos-config confirm >/dev/null && ok "confirm" || bad "confirm"
$S "arcadeos-config set 'ntp.servers=evil;reboot'" >/dev/null 2>&1 && bad "ungueltiger Server angenommen" || ok "ungueltiger Server abgelehnt"

echo "== agentd zeigt Zeit und Netz"
R=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"system_status","arguments":{}}}' | $S agentd)
T=$(printf %s "$R" | python3 -I -c "import json,sys; r=json.load(sys.stdin); d=json.loads(r['result']['content'][0]['text']); print(d['network']['congestion'], d['time']['ntp'].get('synced'), 'fq' in d['network']['qdisc'])")
check "$T" "bbr True True" "system_status: Netz und NTP"

finish
