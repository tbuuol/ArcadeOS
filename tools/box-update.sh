#!/bin/sh
#
# box-update.sh – ein Update-Paket auf dem Geraet selbst bauen, ohne Buildroot.
#
#   tools/box-update.sh <dateien-dir> <versions-zusatz> [--kernel <bzImage>] [--modules <dir>] [--install] [--reboot]
#
# Nimmt das System des laufenden Slots, legt <dateien-dir> darueber (gleiche Pfade wie im Rootfs,
# z.B. <dir>/usr/sbin/meshd), setzt VERSION=<datum.uhrzeit>-<git>+<versions-zusatz>, packt es neu
# (squashfs wie das Original: zstd, 128 KB Bloecke), nimmt den Kernel des laufenden Slots und signiert
# das Manifest mit dem Schluessel des Geraets (/state/update/keys/box.key, minisign).
# Ergebnis: /state/update/build/update.tar; mit --install gleich 'arcadeos-update install' (inaktiver
# Slot, Probeboot), mit --reboot danach Neustart. Den Probeboot bestaetigt update-healthcheck selbst.
#
# --kernel nimmt statt des laufenden Kernels <bzImage>; --modules ersetzt dann /lib/modules im System komplett
# durch <dir>/lib/modules (aus 'make modules_install INSTALL_MOD_PATH=<dir>'), damit Module und Kernel passen.
# Ein neuer Kernel ohne --modules wird abgelehnt, wenn das System Module hat.
#
# Braucht die Build-Umgebung /state/build (Alpine-chroot mit squashfs-tools, /state/build/enter.sh).
#

set -eu
. /usr/lib/arcadeos/common.sh

FILES=${1:?Aufruf: box-update.sh <dateien-dir> <versions-zusatz> [--kernel <bzImage>] [--modules <dir>] [--install] [--reboot]}
TAG=${2:?Versions-Zusatz fehlt}
shift 2
INSTALL=no; REBOOT=no; KERNEL=""; MODULES=""
while [ $# -gt 0 ]; do
	case "$1" in
		--install) INSTALL=yes;;
		--reboot) INSTALL=yes; REBOOT=yes;;
		--kernel) KERNEL=${2:?--kernel braucht eine Datei}; shift;;
		--modules) MODULES=${2:?--modules braucht ein Verzeichnis}; shift;;
		*) echo "unbekannt: $1" >&2; exit 2;;
	esac
	shift
done
[ -z "$KERNEL" ] || [ -f "$KERNEL" ] || { echo "$KERNEL gibt es nicht" >&2; exit 2; }
[ -z "$MODULES" ] || [ -d "$MODULES/lib/modules" ] || { echo "$MODULES/lib/modules gibt es nicht" >&2; exit 2; }
[ -z "$MODULES" ] || [ -n "$KERNEL" ] || { echo "--modules nur zusammen mit --kernel" >&2; exit 2; }
printf %s "$TAG" | grep -qx '[A-Za-z0-9._-]\{1,40\}' || { echo "Versions-Zusatz nur A-Z a-z 0-9 . _ - (max 40)" >&2; exit 2; }
[ -d "$FILES" ] || { echo "$FILES ist kein Verzeichnis" >&2; exit 2; }

KEY=/state/update/keys/box.key
CH=/state/build/alpine
W=$CH/work
OUT=/state/update/build
[ -f "$KEY" ] || { echo "kein Geraete-Schluessel $KEY" >&2; exit 1; }
[ -x /state/build/enter.sh ] || { echo "keine Build-Umgebung (/state/build/enter.sh)" >&2; exit 1; }

run=$(running_slot); r=$(echo "$run" | tr AB ab)
dev=$(find_part "rootfs-$r")
base=$(sed -n 's/^VERSION=//p' /etc/arcadeos-release)
git=${base#*-}; git=${git%%+*}
ver="$(date -u +%Y%m%d.%H%M)-$git+$TAG"
echo "Basis: Slot $run ($base) -> $ver" >&2

mkdir -p "$W"; rm -rf "$W/root" "$W/rootfs.squashfs" "$W/base.img"
dd if="/dev/$dev" of="$W/base.img" bs=1M 2>/dev/null
/state/build/enter.sh unsquashfs -q -d /work/root /work/base.img >/dev/null
rm -f "$W/base.img"
# Dateien darueberlegen: Rechte wie in <dateien-dir>, Besitzer root
(cd "$FILES" && find . -type f) | while read -r f; do
	install -D -m "$(stat -c %a "$FILES/$f")" "$FILES/$f" "$W/root/${f#./}"
	echo "  + ${f#./}" >&2
done
# Neuer Kernel: Module muessen zu ihm passen
if [ -n "$KERNEL" ]; then
	if [ -n "$MODULES" ]; then
		rm -rf "$W/root/lib/modules"
		cp -a "$MODULES/lib/modules" "$W/root/lib/modules"
		echo "  + lib/modules ($(find "$W/root/lib/modules" -name '*.ko*' | wc -l) Module, $(ls "$W/root/lib/modules"))" >&2
	elif [ -n "$(find "$W/root/lib/modules" -name '*.ko*' 2>/dev/null | head -1)" ]; then
		echo "neuer Kernel, aber keine Module (--modules): die alten wuerden nicht passen" >&2; exit 2
	fi
fi
sed -i "s/^VERSION=.*/VERSION=$ver/" "$W/root/etc/arcadeos-release"
/state/build/enter.sh mksquashfs /work/root /work/rootfs.squashfs -comp zstd -b 131072 -noappend -quiet -no-progress
rm -rf "$W/root"

rm -rf "$OUT"; mkdir -p "$OUT"; chmod 700 "$OUT"
mv "$W/rootfs.squashfs" "$OUT/"
bootdev=$(find_part boot); mnt=/run/box-update-boot
mkdir -p $mnt; mount -t vfat -o ro "/dev/$bootdev" $mnt
if [ -n "$KERNEL" ]; then cp "$KERNEL" "$OUT/bzImage"; echo "  + Kernel $KERNEL" >&2; else cp "$mnt/$r/bzImage" "$OUT/bzImage"; fi
umount $mnt; rmdir $mnt
cd "$OUT"
printf 'version=%s\narch=%s\nrootfs_sha256=%s\nkernel_sha256=%s\n' "$ver" "$(uname -m)" \
	"$(sha256sum < rootfs.squashfs | cut -d' ' -f1)" "$(sha256sum < bzImage | cut -d' ' -f1)" > manifest
minisign -S -q -s "$KEY" -m manifest -x manifest.box-arcadeos.minisig -t "ArcadeOS $ver, gebaut auf $(hostname)"
tar c -f update.tar manifest manifest.box-arcadeos.minisig rootfs.squashfs bzImage
rm -f rootfs.squashfs bzImage
echo "$OUT/update.tar ($ver)" >&2
if [ $INSTALL = yes ]; then
	arcadeos-update install "$OUT/update.tar"
	if [ $REBOOT = yes ]; then sync; (sleep 3; reboot) >/dev/null 2>&1 & fi
fi
