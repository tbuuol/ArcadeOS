# Schlüssel

## SSH: `authorized_keys`
Public Keys, die sich als root einloggen dürfen (nicht im Repo). Der Build bricht ab, wenn die
Datei fehlt – ohne Key gäbe es keinen Zugang.

## Update-Signaturen (minisign, Ed25519)
Geräte installieren nur Update-Pakete mit gültiger Signatur. Auf dem Gerät liegen nur Public Keys.

| Ordner              | Wer signiert                         | Gilt für                       |
|---------------------|--------------------------------------|--------------------------------|
| `trusted/release/`  | der User, mit seinem eigenen Schlüssel (liegt nur bei ihm) | alle Builds |
| `trusted/dev/`      | Build-Key auf der Build-Maschine (`~/.config/arcadeos/dev.key`) | nur Dev-Builds (QEMU/Tests) |

Release-Build (`BR2_ARCADEOS_RELEASE=y`): nur `trusted/release/`; ohne Release-Key bricht der Build ab.

Eigenen Release-Key anlegen (auf deinem Rechner, Passwort setzen):

    minisign -G -p arcadeos-release.pub -s ~/.minisign/arcadeos-release.key

`arcadeos-release.pub` nach `keys/trusted/release/` legen (Public Key, darf ins Repo).
Paket signieren: `tools/sign-update.sh output/images/update.tar ~/.minisign/arcadeos-release.key`
