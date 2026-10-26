#!/bin/sh
# Dauerhaft laufende ArcadeOS-VM auf diesem Server (systemd-User-Dienst arcadeos-live).
# Die VM hat eine eigene Platte unter ~/arcadeos/live/ – neue Builds aendern sie nicht; aktualisiert
# wird sie wie ein echtes Geraet mit arcadeos-update. SSH: ssh -p 2223 -i <key> root@127.0.0.1
#
#   tools/live-vm.sh install     Platte anlegen (aus output/images/disk.img), Dienst einrichten + starten
#   tools/live-vm.sh status      Dienst + meshd-Zustand
#   tools/live-vm.sh log [N]     letzte N Zeilen des meshd-Debug-Logs (Standard 50)
#   tools/live-vm.sh stop        Dienst stoppen
#   tools/live-vm.sh uninstall   Dienst entfernen (Platte bleibt in ~/arcadeos/live/)
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIVE="$HOME/arcadeos/live"
UNIT="$HOME/.config/systemd/user/arcadeos-live.service"
KEY=${KEY:-$HOME/.ssh/aios_ed25519}
S="timeout 30 ssh -p 2223 -i $KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5 -o BatchMode=yes root@127.0.0.1"

case "${1:-}" in
	install)
		if [ -e "$LIVE/disk.img" ]; then
			echo "Platte existiert schon ($LIVE/disk.img) – bleibt erhalten"
		else
			mkdir -p "$LIVE"
			cp "$ROOT/output/images/disk.img" "$LIVE/disk.img"
			cp /usr/share/OVMF/OVMF_VARS_4M.fd "$LIVE/ovmf-vars.fd"
			echo "Platte angelegt: $LIVE/disk.img"
		fi
		mkdir -p "$(dirname "$UNIT")"
		cp "$ROOT/tools/arcadeos-live.service" "$UNIT"
		systemctl --user daemon-reload
		systemctl --user enable --now arcadeos-live.service
		systemctl --user --no-pager status arcadeos-live.service | head -5
		;;
	status)
		systemctl --user --no-pager status arcadeos-live.service | head -5 || true
		$S 'meshd status' || echo "(VM nicht per SSH erreichbar)"
		;;
	log)
		$S "tail -n ${2:-50} /state/mesh/debug.log"
		;;
	stop)
		systemctl --user stop arcadeos-live.service
		;;
	uninstall)
		systemctl --user disable --now arcadeos-live.service || true
		rm -f "$UNIT"
		systemctl --user daemon-reload
		echo "Dienst entfernt; Platte bleibt in $LIVE/"
		;;
	*)
		sed -n '2,12s/^# \{0,1\}//p' "$0"; exit 1 ;;
esac
