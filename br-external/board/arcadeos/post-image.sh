#!/bin/sh
# Baut images/disk.img aus genimage.cfg. Argumente: $1 = BINARIES_DIR, $2 = BR2_EXTERNAL_ARCADEOS_PATH
set -eu
exec support/scripts/genimage.sh -c "$2/board/arcadeos/genimage.cfg"
