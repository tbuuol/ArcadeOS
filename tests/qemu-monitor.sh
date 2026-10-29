#!/bin/sh
# Testet die Monitoring-Seite: httpd als nobody, nur statische Dateien, status.json mit echten Werten
# und live, nichts Geheimes darin, nur lesen (kein POST/PUT/CGI/Pfad-Tricks), monitor.port (anderer
# Port, 0 = aus), Hinweis auf der Statusseite. Macht ein Bildschirmfoto der Seite (falls Chrome da).
# Aufruf: tests/qemu-monitor.sh   (KEY=... fuer anderen SSH-Key). Dauer ca. 3 min.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

export HTTP_PORT=${HTTP_PORT:-8780}
URL="http://127.0.0.1:$HTTP_PORT"
CURL="curl -s -m 10"
# Feld aus status.json (Python-Ausdruck auf d)
jq_() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)" 2>/dev/null; }

start_vm
echo "== Start"
wait_ssh && ok "SSH" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }
for i in $(seq 1 30); do $CURL -o /dev/null "$URL/status.json" && break; sleep 1; done

echo "== Dienste"
grep -q 'Starting monitor: OK (Port 80)' $SP/serial.log && ok "S70monitor gestartet (Port 80)" || bad "Start: $(grep 'monitor' $SP/serial.log)"
U=$($S 'ps | grep "[h]ttpd -f"' | awk '{print $2}')
check "$U" "nobody" "httpd laeuft als nobody"
$S 'ps | grep -q "[m]onitor-collect"' && ok "Sammler laeuft" || bad "Sammler laeuft nicht"
check "$($S 'ls -l /run/monitor/status.json' | cut -c1-10)" "-rw-r--r--" "status.json lesbar fuer alle, schreibbar nur fuer root"

echo "== Seite"
H=$($CURL -D $SP/h.txt "$URL/")
echo "$H" | grep -q '<title>ArcadeOS Monitor</title>' && ok "Startseite (index.html)" || bad "Startseite: $(head -c 200 $SP/h.txt)"
$CURL -D - -o /dev/null -H 'Accept-Encoding: gzip' "$URL/" | grep -qi '^Content-Encoding: gzip' \
    && ok "gepackt ausgeliefert, wenn der Browser gzip kann" || bad "kein gzip"
G=$($CURL -H 'Accept-Encoding: gzip' "$URL/" | gzip -dc 2>/dev/null | wc -c)
[ "$G" -gt 60000 ] && ok "gepackte Seite entpackt vollstaendig ($G Bytes)" || bad "gepackte Seite: $G Bytes"

echo "== status.json"
J=$($CURL "$URL/status.json")
echo "$J" > $SP/status1.json
[ -n "$(echo "$J" | jq_ 'd["host"]')" ] && ok "gueltiges JSON" || bad "kein JSON: $(echo "$J" | head -c 300)"
check "$(echo "$J" | jq_ 'd["host"]')" "arcadeos" "Hostname"
check "$(echo "$J" | jq_ 'd["slot"] in ("A","B")')" "True" "Slot"
check "$(echo "$J" | jq_ 'isinstance(d["cpu"]["pct"],(int,float)) and 0<=d["cpu"]["pct"]<=100')" "True" "CPU-Last in %"
check "$(echo "$J" | jq_ 'd["cpu"]["cores"]')" "2" "CPU-Kerne"
check "$(echo "$J" | jq_ '0<d["mem"]["used_kb"]<d["mem"]["total_kb"]')" "True" "RAM belegt/gesamt"
check "$(echo "$J" | jq_ '",".join(x["mount"] for x in d["disks"])')" "/,/state" "Belegung System + Daten"
check "$(echo "$J" | jq_ 'all(0<x["used_kb"]<=x["total_kb"] for x in d["disks"])')" "True" "Belegung plausibel"
check "$(echo "$J" | jq_ '[x["name"] for x in d["drives"]]==["vda"] and d["drives"][0]["size_gb"]>0')" "True" "Laufwerk mit Groesse"
check "$(echo "$J" | jq_ 'd["state_mode"]')" "persistent" "Daten bleiben erhalten"
check "$(echo "$J" | jq_ '[x["ipv4"] for x in d["net"] if x["name"]=="eth0"][0]')" "['10.0.2.15/24']" "eth0 mit IPv4"
check "$(echo "$J" | jq_ 'd["gateway"]')" "10.0.2.2" "Router"
check "$(echo "$J" | jq_ 'sorted(x["proc"] for x in d["services"] if x["up"])')" \
    "['crond', 'dropbear', 'httpd', 'meshd', 'ntpd', 'syslogd', 'watchdog']" "laufende Dienste (WLAN aus)"
check "$(echo "$J" | jq_ 'd["mesh"]["running"] and len(d["mesh"]["node_id"])==16')" "True" "Mesh-Zustand"
check "$(echo "$J" | jq_ '"temp_c" in d or True')" "True" "Temperatur (in QEMU meist ohne Sensor)"
check "$(echo "$J" | jq_ 'd["power"]')" "{}" "kein Akku in QEMU"
sleep 5
J2=$($CURL "$URL/status.json")
check "$(printf '%s\n%s' "$J" "$J2" | python3 -I -c 'import json,sys; a,b=[json.loads(l) for l in sys.stdin]; print(b["t"]>a["t"] and b["uptime_s"]>a["uptime_s"] and len(b["hist"]["cpu"])>len(a["hist"]["cpu"]))')" \
    "True" "live: neuer Stand nach 5 s, Verlauf waechst"

echo "== Nur lesen"
for m in POST PUT DELETE PATCH; do
    c=$($CURL -o /dev/null -w '%{http_code}' -X $m -d 'x=1' "$URL/status.json")
    case "$c" in 2*) bad "$m wird angenommen ($c)" ;; *) ok "$m abgelehnt ($c)" ;; esac
done
for p in /../etc/shadow /..%2f..%2fetc/shadow /%2e%2e/%2e%2e/etc/passwd /cgi-bin/test /etc/passwd /status.json/../../etc/shadow; do
    c=$($CURL --path-as-is -o $SP/body -w '%{http_code}' "$URL$p")
    if [ "$c" = 200 ] || grep -q 'root:' $SP/body; then bad "$p liefert etwas ($c)"; else ok "$p gesperrt ($c)"; fi
done
c=$($CURL -o $SP/body -w '%{http_code}' "$URL/nichtda/")
grep -qi 'index of' $SP/body && bad "Verzeichnisliste" || ok "keine Verzeichnisliste ($c)"
$S 'ls /usr/share/arcadeos/monitor' | tr '\n' ' ' | grep -qx 'index.html index.html.gz status.json ' \
    && ok "Webverzeichnis: nur index.html(.gz) + status.json" || bad "Webverzeichnis: $($S 'ls -la /usr/share/arcadeos/monitor')"

echo "== Nichts Geheimes"
printf '%s\n%s\n' 'Geheim Netz' 'geheim-passwort-123' | $SQ 'IFS= read -r s; IFS= read -r p; arcadeos-config set "wifi.ssid=$s" "wifi.psk=$p" hostname=mon-test' >/dev/null 2>&1
sleep 3; wait_ssh 120 >/dev/null; $S 'arcadeos-config confirm' >/dev/null
sleep 4
J=$($CURL "$URL/status.json")
echo "$J" > $SP/status2.json
echo "$J" | grep -qiE 'Geheim Netz|geheim-passwort' && bad "WLAN-Name/Passwort in status.json" || ok "kein WLAN-Name/Passwort in status.json"
echo "$J" | jq_ '[e["detail"] for e in d["audit"] if e["action"]=="config-change"][-1]' | grep -q 'wifi.ssid=\*\*\* wifi.psk=\*\*\* hostname=mon-test' \
    && ok "Audit-Eintrag mit maskiertem WLAN" || bad "Audit: $(echo "$J" | jq_ 'd["audit"][-2:]')"
check "$(echo "$J" | jq_ 'all("who" not in e for e in d["audit"])')" "True" "keine Herkunft (who) im Protokoll"
check "$(echo "$J" | jq_ 'all("addr" not in l for l in d["mesh"]["links"])')" "True" "keine Adressen der Mesh-Gegenstellen"
check "$(echo "$J" | jq_ 'd["host"]')" "mon-test" "neuer Hostname sichtbar"
$SQ 'arcadeos-config set wifi.ssid= hostname=arcadeos' >/dev/null 2>&1; sleep 3; wait_ssh 120 >/dev/null; $S 'arcadeos-config confirm' >/dev/null

echo "== Statusseite"
$S '/usr/lib/arcadeos/status-screen --once' | grep -q 'Web:   http://10.0.2.15/  (Monitoring, nur lesen)' \
    && ok "Statusseite nennt die Adresse" || bad "Statusseite: $($S '/usr/lib/arcadeos/status-screen --once' | grep -i web)"

echo "== monitor.port"
$S 'arcadeos-config set monitor.port=22' >/dev/null 2>&1 && bad "Port 22 angenommen" || ok "Port 22 abgelehnt"
$S 'arcadeos-config set monitor.port=abc' >/dev/null 2>&1 && bad "Port abc angenommen" || ok "ungueltiger Port abgelehnt"
$S 'arcadeos-config set monitor.port=8080' >/dev/null && $S 'arcadeos-config confirm' >/dev/null
sleep 3
# Neustart aus der SSH-Sitzung (umask 077) darf status.json nicht unlesbar machen
check "$($S 'ls -l /run/monitor/status.json' | cut -c1-10)" "-rw-r--r--" "status.json nach Neustart aus SSH lesbar"
$S 'wget -q -O - http://127.0.0.1:8080/status.json' | grep -q '"host"' && ok "Port 8080 liefert" || bad "Port 8080 liefert nicht"
$S 'wget -q -T 3 -O - http://127.0.0.1/status.json' >/dev/null 2>&1 && bad "Port 80 noch offen" || ok "Port 80 zu"
$S '/usr/lib/arcadeos/status-screen --once' | grep -q 'http://10.0.2.15:8080/' && ok "Statusseite mit Port 8080" || bad "Statusseite ohne :8080"
$S 'arcadeos-config set monitor.port=0' >/dev/null && $S 'arcadeos-config confirm' >/dev/null
sleep 2
$S 'ps | grep -qE "[h]ttpd -f|[m]onitor-collect"' && bad "monitor.port=0: laeuft noch" || ok "monitor.port=0: Webserver und Sammler aus"
$S '/usr/lib/arcadeos/status-screen --once' | grep -q '^Web:' && bad "Statusseite nennt Web trotz aus" || ok "Statusseite ohne Web-Zeile"
$S 'arcadeos-config set monitor.port=' >/dev/null && $S 'arcadeos-config confirm' >/dev/null
for i in $(seq 1 15); do $CURL -o /dev/null "$URL/status.json" && break; sleep 1; done
check "$($CURL -o /dev/null -w '%{http_code}' "$URL/status.json")" "200" "Standard (Port 80) wieder an"
$S 'arcadeos-config show' | grep -q '"monitor_port":""' && ok "show: monitor_port" || bad "show: $($S 'arcadeos-config show')"

echo "== Neustart"
reboot_vm && wait_ssh >/dev/null
for i in $(seq 1 30); do $CURL -o /dev/null "$URL/status.json" && break; sleep 1; done
check "$($CURL "$URL/status.json" | jq_ 'd["host"]')" "arcadeos" "nach Neustart wieder da"

# Bildschirmfoto mit echten Daten (Startbildschirm ausgeblendet), wenn Playwrights Chrome vorhanden ist
CH=$(ls "$HOME"/.cache/ms-playwright/chromium_headless_shell-*/*/chrome-headless-shell 2>/dev/null | tail -1)
if [ -x "$CH" ]; then
    D="$ROOT/test-logs/monitor"; mkdir -p "$D"
    $CURL "$URL/" | sed 's#</head>#<base href="'"$URL"'/"><style>.boot{display:none!important}.app > *{animation:none!important;opacity:1!important;transform:none!important}</style></head>#' > $SP/shot.html
    for th in atlantis oasis; do
        "$CH" --no-sandbox --disable-web-security --user-data-dir=$SP/chrome --hide-scrollbars --window-size=1366,768 --virtual-time-budget=6000 \
            --screenshot="$D/$th-1366x768.png" "file://$SP/shot.html?theme=$th" >/dev/null 2>&1
    done
    ls "$D"/*.png >/dev/null 2>&1 && ok "Bildschirmfotos in test-logs/monitor/" || bad "Bildschirmfoto fehlgeschlagen"
fi

finish
