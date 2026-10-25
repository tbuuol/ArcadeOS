#!/bin/sh
# Erzeugt board/arcadeos/linux.config aus tinyconfig + linux-fragment.config.
# Aufruf: gen-linux-config.sh <entpackter-kernel-quellbaum>
# Braucht flex/bison im PATH (z.B. output/host/bin von Buildroot).
set -eu
SRC="$1"
BOARD="$(cd "$(dirname "$0")" && pwd)"
cd "$SRC"
make -s ARCH=x86_64 tinyconfig
scripts/kconfig/merge_config.sh -m .config "$BOARD/linux-fragment.config" >/dev/null
make -s ARCH=x86_64 olddefconfig
# Pruefen, dass jede Option aus dem Fragment auch wirklich so gesetzt ist
MISSING=$(grep -E '^CONFIG_[A-Z0-9_]+=' "$BOARD/linux-fragment.config" | while IFS== read -r k v; do
    if [ "$v" = n ]; then
        if grep -q "^$k=" .config; then echo "FEHLT: $k=n, ist $(grep "^$k=" .config)"; fi
    else
        grep -qx "$k=$v" .config || echo "FEHLT: $k=$v"
    fi
done)
make -s ARCH=x86_64 savedefconfig
cp defconfig "$BOARD/linux.config"
if [ -n "$MISSING" ]; then echo "$MISSING"; exit 1; fi
echo "linux.config ok ($(wc -l < "$BOARD/linux.config") Zeilen)"
