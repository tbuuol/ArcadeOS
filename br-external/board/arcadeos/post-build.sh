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
echo "ArcadeOS phase0 $(date -u +%Y-%m-%dT%H:%MZ)" > "$TARGET_DIR/etc/arcadeos-release"

# root ohne nutzbares Passwort: kein Login an der Konsole, nur SSH-Key
sed -i 's/^root:[^:]*:/root:*:/' "$TARGET_DIR/etc/shadow"
