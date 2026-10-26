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
