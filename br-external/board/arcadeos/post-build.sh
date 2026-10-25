#!/bin/sh
# Installiert die SSH-Keys aus keys/authorized_keys im Repo fuer root.
set -eu
EXT="$2"
KEYS="$EXT/../keys/authorized_keys"
if [ ! -s "$KEYS" ]; then
    echo "post-build: $KEYS fehlt - ohne Key kein Login" >&2
    exit 1
fi
install -d -m 0700 "$TARGET_DIR/root/.ssh"
install -m 0600 "$KEYS" "$TARGET_DIR/root/.ssh/authorized_keys"
# Version: Datum + Git-Stand des Repos (+dirty bei uncommitteten Aenderungen); landet auch im Update-Manifest
REV=$(git -C "$EXT/.." describe --always --dirty 2>/dev/null || echo local)
cat > "$TARGET_DIR/etc/arcadeos-release" <<REL
NAME=ArcadeOS
VERSION=$(date -u +%Y%m%d.%H%M)-$REV
ARCH=$(sed -n 's/^BR2_ARCH="\(.*\)"/\1/p' "$BR2_CONFIG")
REL

# root ohne nutzbares Passwort: kein Login an der Konsole, nur SSH-Key
sed -i 's/^root:[^:]*:/root:*:/' "$TARGET_DIR/etc/shadow"

# Read-only Rootfs: persistente Pfade zeigen auf die Zustands-Partition (S00state)
install -d "$TARGET_DIR/state" "$TARGET_DIR/boot"
rm -rf "$TARGET_DIR/etc/dropbear"
ln -s /state/dropbear "$TARGET_DIR/etc/dropbear"
