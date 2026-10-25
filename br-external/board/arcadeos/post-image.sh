#!/bin/sh
# Baut images/disk.img aus genimage.cfg. Argumente: $1 = BINARIES_DIR, $2 = BR2_EXTERNAL_ARCADEOS_PATH
set -eu
BOARD="$2/board/arcadeos"
EFI="$1/efi-part/EFI/BOOT"

# Eigene GRUB-Konfiguration statt der Buildroot-Vorlage
install -m 0644 "$BOARD/grub.cfg" "$EFI/grub.cfg"

# grubenv: 1024-Byte-Block, mit '#' aufgefuellt (Format von grub-editenv)
{ printf '# GRUB Environment Block\nslot=A\n'; head -c 1024 /dev/zero | tr '\0' '#'; } | head -c 1024 > "$EFI/grubenv"

exec support/scripts/genimage.sh -c "$BOARD/genimage.cfg"
