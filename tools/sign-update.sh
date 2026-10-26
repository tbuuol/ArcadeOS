#!/bin/sh
# Signiert ein ArcadeOS-Update-Paket mit einem minisign-Schluessel (z.B. deinem Release-Key).
# Die Signatur kommt als manifest.<name>.minisig ins Paket; das Manifest enthaelt die
# Pruefsummen von System und Kernel, damit ist das ganze Paket abgedeckt.
#
#   tools/sign-update.sh update.tar ~/.minisign/arcadeos-release.key [name]
#
# Braucht minisign (https://jedisct1.github.io/minisign/). name: Standard = Dateiname des Keys.
set -eu
PKG=${1:?Aufruf: sign-update.sh <update.tar> <minisign-secret-key> [name]}
KEY=${2:?Aufruf: sign-update.sh <update.tar> <minisign-secret-key> [name]}
NAME=${3:-$(basename "$KEY" .key)}
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
tar -x -C "$TMP" -f "$PKG" manifest
VER=$(sed -n 's/^version=//p' "$TMP/manifest")
echo "Signiere ArcadeOS $VER mit $KEY ..."
minisign -S -s "$KEY" -m "$TMP/manifest" -x "$TMP/manifest.$NAME.minisig" -t "ArcadeOS $VER $NAME"
# alte Signatur gleichen Namens ersetzen, neue anhaengen
if tar -tf "$PKG" | grep -qx "manifest.$NAME.minisig"; then
    mkdir "$TMP/all"; tar -x -C "$TMP/all" -f "$PKG"
    cp "$TMP/manifest.$NAME.minisig" "$TMP/all/"
    (cd "$TMP/all" && tar -c -f "$TMP/new.tar" manifest manifest.*.minisig rootfs.squashfs bzImage)
    cp "$TMP/new.tar" "$PKG"
else
    tar -r -f "$PKG" -C "$TMP" "manifest.$NAME.minisig"
fi
echo "OK: $(tar -tf "$PKG" | grep minisig | tr '\n' ' ')"
