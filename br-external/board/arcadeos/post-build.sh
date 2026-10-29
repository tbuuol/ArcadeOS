#!/bin/sh
# Installiert die SSH-Keys aus keys/authorized_keys im Repo fuer root.
set -eu
EXT="$2"
KEYS="$EXT/../keys/authorized_keys"
if [ ! -s "$KEYS" ]; then
    echo "post-build: $KEYS fehlt - ohne Key kein Login" >&2
    exit 1
fi
# Keys aus dem Image = Recovery-Keys: gelten immer, zusaetzlich zu /state/config/authorized_keys.
# apply-config (S10config) erzeugt daraus /run/arcadeos/authorized_keys.
install -d -m 0755 "$TARGET_DIR/etc/arcadeos"
install -m 0644 "$KEYS" "$TARGET_DIR/etc/arcadeos/recovery_keys"
install -d -m 0700 "$TARGET_DIR/root/.ssh"
rm -f "$TARGET_DIR/root/.ssh/authorized_keys"
ln -s /run/arcadeos/authorized_keys "$TARGET_DIR/root/.ssh/authorized_keys"

# Netzwerk-Konfiguration erzeugt apply-config aus /state/config
rm -f "$TARGET_DIR/etc/network/interfaces"
ln -s /run/arcadeos/interfaces "$TARGET_DIR/etc/network/interfaces"
# Version: Datum + Git-Stand des Repos (+dirty bei uncommitteten Aenderungen); landet auch im Update-Manifest
REV=$(git -C "$EXT/.." describe --always --dirty 2>/dev/null || echo local)
if grep -qx 'BR2_ARCADEOS_RELEASE=y' "$BR2_CONFIG"; then CHANNEL=release; else CHANNEL=dev; fi
cat > "$TARGET_DIR/etc/arcadeos-release" <<REL
NAME=ArcadeOS
VERSION=$(date -u +%Y%m%d.%H%M)-$REV
ARCH=$(sed -n 's/^BR2_ARCH="\(.*\)"/\1/p' "$BR2_CONFIG")
CHANNEL=$CHANNEL
REL

# Public Keys fuer Update-Signaturen: Release-Keys immer, Build-Keys nur im Dev-Build
UK="$TARGET_DIR/etc/arcadeos/update-keys"
rm -rf "$UK"; install -d "$UK"
for k in "$EXT/../keys/trusted/release/"*.pub; do
    if [ -f "$k" ]; then install -m 0644 "$k" "$UK/release-$(basename "$k")"; fi
done
if [ "$CHANNEL" = dev ]; then
    for k in "$EXT/../keys/trusted/dev/"*.pub; do
        if [ -f "$k" ]; then install -m 0644 "$k" "$UK/dev-$(basename "$k")"; fi
    done
fi
if [ -z "$(ls "$UK")" ]; then
    echo "post-build: keine Update-Keys fuer Kanal '$CHANNEL' (keys/trusted/) - Updates waeren unmoeglich" >&2
    exit 1
fi

# root ohne nutzbares Passwort: kein Login an der Konsole, nur SSH-Key
sed -i 's/^root:[^:]*:/root:*:/' "$TARGET_DIR/etc/shadow"

# Read-only Rootfs: persistente Pfade zeigen auf die Zustands-Partition (S00state)
install -d "$TARGET_DIR/state" "$TARGET_DIR/boot"
rm -rf "$TARGET_DIR/etc/dropbear"
ln -s /state/dropbear "$TARGET_DIR/etc/dropbear"

# Nicht gebraucht auf dem Geraet (agentd laeuft mit dem Lua-Interpreter)
rm -f "$TARGET_DIR/usr/bin/luac" "$TARGET_DIR/usr/bin/lua2json" "$TARGET_DIR/usr/bin/json2lua"

# Intel-WLAN-Firmware: linux-firmware liefert je Chip mehrere API-Versionen (je ~2,4 MB), der Kernel
# laedt die hoechste, die er kennt - aeltere weg, danach verwaiste Links entfernen
FW="$TARGET_DIR/lib/firmware"
if [ -d "$FW/intel/iwlwifi" ]; then
    for chip in $(ls "$FW/intel/iwlwifi" | sed -n 's/^\(iwlwifi-.*\)-[0-9]*\.ucode$/\1/p' | sort -u); do
        ls "$FW/intel/iwlwifi/$chip"-*.ucode | sed -n "s/.*-\([0-9]*\)\.ucode$/\1/p" | sort -n | head -n -1 | \
            while read -r v; do rm -f "$FW/intel/iwlwifi/$chip-$v.ucode"; done
    done
    find "$FW" -xtype l -delete
fi

# Monitoring-Seite: status.json schreibt monitor-collect nach /run (tmpfs), httpd liefert index.html.gz
# an Browser, die gzip koennen
M="$TARGET_DIR/usr/share/arcadeos/monitor"
if [ -f "$M/index.html" ]; then
    gzip -9 -n -c "$M/index.html" > "$M/index.html.gz"
    ln -sfn /run/monitor/status.json "$M/status.json"
fi
