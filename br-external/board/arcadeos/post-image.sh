#!/bin/sh
# Baut images/disk.img aus genimage.cfg. Argumente: $1 = BINARIES_DIR, $2 = BR2_EXTERNAL_ARCADEOS_PATH
set -eu
BOARD="$2/board/arcadeos"
EFI="$1/efi-part/EFI/BOOT"

# Eigene GRUB-Konfiguration statt der Buildroot-Vorlage
install -m 0644 "$BOARD/grub.cfg" "$EFI/grub.cfg"
# Hinweis fuer den User auf der FAT-Partition (SSH-Key ablegen)
install -m 0644 "$BOARD/LIESMICH.txt" "$1/LIESMICH.txt"
install -m 0644 "$BOARD/README.txt" "$1/README.txt"

# grubenv: 1024-Byte-Block, mit '#' aufgefuellt (Format von grub-editenv)
{ printf '# GRUB Environment Block\nslot=A\n'; head -c 1024 /dev/zero | tr '\0' '#'; } | head -c 1024 > "$EFI/grubenv"

# BIOS-Start (alte PCs ohne UEFI): der GRUB-Kern (grub.img) liegt in der Partition bios-boot ab
# 1 MiB (LBA 2048), der Startcode (boot.img) im ersten Sektor der Platte. Beide enthalten die
# Sektornummer des naechsten Teils - das macht sonst grub-install, hier passend zu genimage.cfg.
BOOTIMG=$(ls "$BUILD_DIR"/grub2-*/build-i386-pc/grub-core/boot.img | head -n1)
python3 - "$BOOTIMG" "$1/grub.img" "$1/boot-mbr.img" 2048 <<'PY'
import struct, sys
boot = bytearray(open(sys.argv[1], "rb").read())
core = bytearray(open(sys.argv[2], "rb").read())
lba = int(sys.argv[4])
assert len(boot) == 512 and len(core) <= 1024 * 1024, "boot.img/grub.img unerwartet gross"
# boot.img: 64-Bit-Sektornummer von core.img bei 0x5c (Vorgabe 1 = direkt hinter dem MBR)
assert struct.unpack_from("<Q", boot, 0x5C)[0] in (1, lba)
struct.pack_into("<Q", boot, 0x5C, lba)
# diskboot (erster Sektor von core.img): Blockliste am Sektorende, Start des Rests (Vorgabe 2)
assert struct.unpack_from("<Q", core, 0x1F4)[0] in (2, lba + 1)
struct.pack_into("<Q", core, 0x1F4, lba + 1)
open(sys.argv[3], "wb").write(boot)
open(sys.argv[2], "wb").write(core)
PY

support/scripts/genimage.sh -c "$BOARD/genimage.cfg"

# Update-Paket fuer arcadeos-update: manifest + rootfs + Kernel
PKG="$1/update-pkg"
rm -rf "$PKG"; mkdir -p "$PKG"
cp "$1/rootfs.squashfs" "$1/bzImage" "$PKG/"
{
	echo "version=$(sed -n 's/^VERSION=//p' "$TARGET_DIR/etc/arcadeos-release")"
	echo "arch=$(sed -n 's/^ARCH=//p' "$TARGET_DIR/etc/arcadeos-release")"
	echo "rootfs_sha256=$(sha256sum < "$PKG/rootfs.squashfs" | cut -d' ' -f1)"
	echo "kernel_sha256=$(sha256sum < "$PKG/bzImage" | cut -d' ' -f1)"
} > "$PKG/manifest"

# Dev-Build: Manifest mit dem Build-Key signieren (Release-Pakete signiert der User: tools/sign-update.sh)
CHANNEL=$(sed -n 's/^CHANNEL=//p' "$TARGET_DIR/etc/arcadeos-release")
DEVKEY="$HOME/.config/arcadeos/dev.key"
if [ "$CHANNEL" = dev ] && [ -f "$DEVKEY" ]; then
	"$HOST_DIR/bin/minisign" -S -s "$DEVKEY" -m "$PKG/manifest" -x "$PKG/manifest.dev.minisig" \
		-t "ArcadeOS $(sed -n 's/^version=//p' "$PKG/manifest") dev" >/dev/null
elif [ "$CHANNEL" = dev ]; then
	echo "WARNUNG: $DEVKEY fehlt - update.tar ist unsigniert und wird von Geraeten abgelehnt" >&2
fi
SIGS=$(cd "$PKG" && ls manifest.*.minisig 2>/dev/null || true)
tar -c -f "$1/update.tar" -C "$PKG" manifest $SIGS rootfs.squashfs bzImage
echo "Update-Paket: $1/update.tar ($(sed -n 's/^version=//p' "$PKG/manifest"))"
rm -rf "$PKG"
