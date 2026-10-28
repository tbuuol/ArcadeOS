#!/bin/sh
# Testet WLAN in QEMU mit simulierten Funkkarten (mac80211_hwsim): wlan1 spielt den Router
# (wpa_supplicant im AP-Modus + udhcpd), wlan0 ist die WLAN-Karte von ArcadeOS.
# Geprueft: Module/Regulierungsdatenbank, wifi.txt vom Stick (BOM, CRLF, mehrere Netze),
# wifi.ssid/wifi.psk ueber arcadeos-config (Sonderzeichen), falsches Passwort + Rollback,
# Passwort nirgends im Klartext, Statusseite.
# Aufruf: tests/qemu-wifi.sh   (KEY=... fuer anderen SSH-Key). Dauer ca. 3–5 min.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

# Testnetz: Sonderzeichen in SSID und Passwort, damit Quoting-Fehler auffallen
SSID='Arcade "Test" ü'
PSK='pa$$ wört 123!'
SSID_HEX=$(printf %s "$SSID" | od -An -tx1 -v | tr -d ' \n')

start_vm
echo "== Start"
wait_ssh && ok "SSH" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }

echo "== Ohne WLAN-Hardware"
grep -q 'Loading modules: keine' $SP/serial.log && ok "keine Module geladen (QEMU hat keine WLAN-Karte)" \
    || bad "Module beim Start: $(grep 'Loading modules' $SP/serial.log)"
$S 'test -f /lib/firmware/regulatory.db && test -f /lib/firmware/ath10k/QCA9377/hw1.0/firmware-6.bin && ls /lib/firmware/iwlwifi-8265-*.ucode' >/dev/null 2>&1 \
    && ok "Firmware im Image (QCA9377, Intel 8265, regulatory.db)" || bad "Firmware fehlt: $($S 'ls -R /lib/firmware | head -30')"

echo "== Simulierte Funkkarten"
$S 'modprobe mac80211_hwsim radios=2' && ok "mac80211_hwsim geladen" || bad "modprobe mac80211_hwsim"
for i in $(seq 1 20); do $S 'test -e /sys/class/net/wlan1/phy80211' && break; sleep 1; done
$S 'test -e /sys/class/net/wlan0/phy80211 && test -e /sys/class/net/wlan1/phy80211' && ok "wlan0 und wlan1 da" \
    || bad "keine WLAN-Geraete: $($S 'ls /sys/class/net')"
$S 'dmesg | grep -i regulatory' | grep -qi 'failed' && bad "Regulierungsdatenbank: $($S 'dmesg | grep -i regulatory')" \
    || ok "Regulierungsdatenbank ohne Fehler geladen"
$S '/usr/lib/arcadeos/status-screen --once' | grep -q 'wlan0.*WLAN aus - kein Netz eingetragen' \
    && ok "Statusseite: WLAN ohne Netz" || bad "Statusseite: $($S '/usr/lib/arcadeos/status-screen --once' | grep wlan)"

# nur wlan0 fuer ArcadeOS, wlan1 gehoert dem Test-Router
$S 'arcadeos-config set wifi.interface=wlan0' >/dev/null && $S 'arcadeos-config confirm' >/dev/null \
    && ok "wifi.interface=wlan0" || bad "wifi.interface setzen"

echo "== Test-Router auf wlan1"
$S sh -s <<EOF
mkdir -p /tmp/ap
cat > /tmp/ap/wpa.conf <<'X'
ctrl_interface=/run/wpa_ap
ap_scan=2
network={
	ssid=$SSID_HEX
	mode=2
	frequency=2437
	key_mgmt=WPA-PSK
	proto=RSN
	pairwise=CCMP
	group=CCMP
	psk="$PSK"
}
X
cat > /tmp/ap/udhcpd.conf <<'X'
interface wlan1
start 10.77.0.100
end 10.77.0.150
lease_file /tmp/ap/leases
pidfile /tmp/ap/udhcpd.pid
option subnet 255.255.255.0
X
touch /tmp/ap/leases
ip addr add 10.77.0.1/24 dev wlan1
wpa_supplicant -B -i wlan1 -c /tmp/ap/wpa.conf -P /tmp/ap/wpa.pid >/tmp/ap/wpa.log 2>&1
udhcpd /tmp/ap/udhcpd.conf
EOF
AP=""
for i in $(seq 1 20); do AP=$($S 'wpa_cli -p /run/wpa_ap -i wlan1 status' 2>/dev/null | sed -n 's/^wpa_state=//p'); [ "$AP" = COMPLETED ] && break; sleep 1; done
check "$AP" "COMPLETED" "Test-Router sendet"

# wartet, bis wlan0 verbunden ist und per DHCP eine 10.77.0.x hat; Ausgabe: Zustand und Adresse
wait_wifi() {
    st=""; ad=""
    for i in $(seq 1 ${1:-60}); do
        st=$($S 'wpa_cli -p /run/wpa_supplicant -i wlan0 status' 2>/dev/null | sed -n 's/^wpa_state=//p')
        ad=$($S 'ip -o -4 addr show dev wlan0' 2>/dev/null | awk '{print $4}')
        [ "$st" = COMPLETED ] && case "$ad" in 10.77.0.*) break ;; esac
        sleep 1
    done
    echo "$st $ad"
}

echo "== wifi.txt vom Stick (BOM, Windows-Zeilenenden, zwei Netze, das erste falsch)"
$S sh -s <<EOF
mkdir -p /tmp/bootrw
mount -t vfat /dev/\$(. /usr/lib/arcadeos/common.sh; find_part boot) /tmp/bootrw
printf '\357\273\277ssid=Gibt-es-nicht\r\npassword=irgendwas1\r\n\r\nssid=%s\r\npassword=%s\r\ncountry=DE\r\n' '$SSID' '$PSK' > /tmp/bootrw/wifi.txt
umount /tmp/bootrw
/usr/lib/arcadeos/apply-config boot >/dev/null
ifup -a >/dev/null 2>&1
EOF
$S 'tail -1 /state/log/audit.log' | grep -q '"action":"boot-wifi","detail":"2 Netz' && ok "Audit: 2 Netze vom Stick" \
    || bad "Audit: $($S 'tail -1 /state/log/audit.log')"
check "$($S 'grep -c ^network= /run/arcadeos/wpa_supplicant.conf; grep -c ^country=DE /run/arcadeos/wpa_supplicant.conf' | tr '\n' ' ')" "2 1 " "wpa_supplicant.conf: 2 Netze, Land DE"
R=$(wait_wifi)
case "$R" in "COMPLETED 10.77.0."*) ok "verbunden ueber wifi.txt: $R" ;; *) bad "nicht verbunden: $R"; $S 'grep -i wpa /var/log/messages | tail -20' ;; esac
# (ein Ping ginge nicht: beide Funkkarten sitzen im selben System, der Kernel antwortet lokal)
$S 'wpa_cli -p /run/wpa_ap -i wlan1 all_sta' | grep -q '^02:00:00:00:00:00' \
    && ok "Test-Router fuehrt wlan0 als verbundene Station" || bad "Test-Router: $($S 'wpa_cli -p /run/wpa_ap -i wlan1 all_sta')"
SCR=$($S '/usr/lib/arcadeos/status-screen --once')
echo "$SCR" | grep -qF "WLAN \"$SSID\"" && echo "$SCR" | grep -q 'IPv4 10.77.0.' && echo "$SCR" | grep -q 'ssh root@10.77.0.' \
    && ok "Statusseite: Netzname, IP und SSH-Befehl" || bad "Statusseite: $SCR"
echo "$SCR" | grep -q 'Signal -[0-9]* dBm' && ok "Statusseite: Signalstaerke" || bad "keine Signalstaerke: $(echo "$SCR" | grep wlan0)"
$S 'ifdown wlan0' >/dev/null 2>&1
$S '! test -f /run/wpa_supplicant.wlan0.pid && ! ps | grep -q "[w]pa_supplicant -B -s -i wlan0"' && ok "ifdown beendet wpa_supplicant" \
    || bad "wpa_supplicant laeuft nach ifdown weiter"
# Stick wieder ohne WLAN, sonst verbindet er unten trotz falschem Passwort in der Konfiguration
$S sh -s <<'EOF'
mount -t vfat /dev/$(. /usr/lib/arcadeos/common.sh; find_part boot) /tmp/bootrw && rm -f /tmp/bootrw/wifi.txt && umount /tmp/bootrw
/usr/lib/arcadeos/apply-config boot >/dev/null
EOF
check "$($S 'grep -c ^network= /run/arcadeos/wpa_supplicant.conf')" "0" "ohne wifi.txt kein Netz"

echo "== wifi.ssid / wifi.psk ueber arcadeos-config"
# Werte per stdin an ein Skript in der VM, damit Anfuehrungszeichen und \$ unveraendert ankommen
printf '%s\n%s\n' "$SSID" "$PSK" | $SQ 'IFS= read -r s; IFS= read -r p; arcadeos-config set "wifi.ssid=$s" "wifi.psk=$p" wifi.country=DE' >/dev/null 2>&1
sleep 3; wait_ssh 120 >/dev/null
R=$(wait_wifi)
case "$R" in "COMPLETED 10.77.0."*) ok "verbunden ueber arcadeos-config: $R" ;; *) bad "nicht verbunden: $R" ;; esac
$S 'arcadeos-config confirm' | grep -q '"confirmed":true' && ok "confirm" || bad "confirm"
SHOW=$($S 'arcadeos-config show')
echo "$SHOW" | grep -q '"psk_set":true' && ok "show: psk_set=true" || bad "show: $SHOW"
LEAK=$($S "grep -rlF 'wört' /state /run /var/log 2>/dev/null | grep -vxE '/state/config/system.conf|/run/arcadeos/wpa_supplicant.conf'")
[ -z "$LEAK" ] && ok "Passwort nur in system.conf und wpa_supplicant.conf (nicht im Audit-Log, Syslog, show)" || bad "Passwort im Klartext in: $LEAK"
echo "$SHOW" | grep -qF 'wört' && bad "show gibt das Passwort aus" || ok "show gibt das Passwort nicht aus"
check "$($S 'ls -l /run/arcadeos/wpa_supplicant.conf /state/config/system.conf' | cut -c1-10 | sort -u)" "-rw-------" "wpa_supplicant.conf und system.conf nur fuer root lesbar"

echo "== Falsches Passwort, dann Rollback"
$SQ 'arcadeos-config set wifi.psk=falsch-falsch' >/dev/null 2>&1
sleep 3; wait_ssh 120 >/dev/null
R=$(wait_wifi 25)
case "$R" in "COMPLETED 10.77.0."*) bad "trotz falschem Passwort verbunden: $R" ;; *) ok "falsches Passwort: keine Verbindung ($R)" ;; esac
$SQ 'arcadeos-config rollback' >/dev/null 2>&1
sleep 3; wait_ssh 120 >/dev/null
R=$(wait_wifi)
case "$R" in "COMPLETED 10.77.0."*) ok "nach Rollback wieder verbunden: $R" ;; *) bad "nach Rollback nicht verbunden: $R" ;; esac
$S "arcadeos-config set wifi.psk=kurz" >/dev/null 2>&1 && bad "zu kurzes Passwort angenommen" || ok "zu kurzes Passwort abgelehnt"
$S "arcadeos-config set wifi.country=de" >/dev/null 2>&1 && bad "ungueltiges Land angenommen" || ok "ungueltiges Land abgelehnt"

echo "== WLAN aus der Konfiguration entfernen"
$SQ 'arcadeos-config set wifi.ssid=' >/dev/null 2>&1
sleep 3; wait_ssh 120 >/dev/null
$S 'arcadeos-config confirm' >/dev/null
$S 'ps | grep -q "[w]pa_supplicant -B -s -i wlan0"' && bad "wpa_supplicant laeuft noch" || ok "wpa_supplicant beendet, wlan0 aus"
$S 'ping -c 1 -W 3 10.0.2.2' >/dev/null && ok "Kabelnetz laeuft weiter" || bad "Kabelnetz weg"

finish
