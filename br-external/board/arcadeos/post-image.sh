#!/bin/sh
# Baut images/disk.img aus genimage.cfg. Argumente: $1 = BINARIES_DIR, $2 = BR2_EXTERNAL_ARCADEOS_PATH
set -eu
BOARD="$2/board/arcadeos"
EFI="$1/efi-part/EFI/BOOT"

# Eigene GRUB-Konfiguration statt der Buildroot-Vorlage
install -m 0644 "$BOARD/grub.cfg" "$EFI/grub.cfg"

# grubenv: 1024-Byte-Block, mit '#' aufgefuellt (Format von grub-editenv)
{ printf '# GRUB Environment Block\nslot=A\n'; head -c 1024 /dev/zero | tr '\0' '#'; } | head -c 1024 > "$EFI/grubenv"

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
tar -c -f "$1/update.tar" -C "$PKG" manifest rootfs.squashfs bzImage
echo "Update-Paket: $1/update.tar ($(sed -n 's/^version=//p' "$PKG/manifest"))"
rm -rf "$PKG"
