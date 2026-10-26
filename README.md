# ArcadeOS

Schlankes Linux für einen KI-Betreiber: Linux-Kernel, musl, busybox und dropbear, gebaut mit
[Buildroot](https://buildroot.org). Kein GUI, Zugang nur per SSH-Key. Ziele und Phasen stehen in
[PLAN.md](PLAN.md).

## Bauen

Benötigt (Ubuntu/Debian): `build-essential unzip qemu-system-x86 ovmf`

```sh
wget https://buildroot.org/downloads/buildroot-2026.02.3.tar.xz
tar xf buildroot-2026.02.3.tar.xz
cat ~/.ssh/<dein-key>.pub > keys/authorized_keys
make -C buildroot-2026.02.3 O=$PWD/output BR2_EXTERNAL=$PWD/br-external arcadeos_x86_64_defconfig
make -C output          # ca. 30 min beim ersten Mal
```

Fortschritt während des Builds: `./progress.sh` (nach `make -s -C output show-targets | tr ' ' '\n' > targets.txt`
und `date +%s > build.start`).

## Starten

```sh
./run.sh                                   # serielle Konsole, beenden mit Ctrl-A X
ssh -p 2222 -i ~/.ssh/<dein-key> root@127.0.0.1
```

## Kernel

Eigene, schlanke Config: `make tinyconfig` plus
[linux-fragment.config](br-external/board/arcadeos/linux-fragment.config) (nur virtio, serielle
Konsole, ext4, IPv4/IPv6, keine Module). Nach einer Änderung am Fragment neu erzeugen:

```sh
tar xf buildroot-2026.02.3/dl/linux/linux-6.18.7.tar.xz -C /tmp
PATH=$PWD/output/host/bin:$PATH br-external/board/arcadeos/gen-linux-config.sh /tmp/linux-6.18.7
```

Das Skript prüft, dass jede Option aus dem Fragment wirklich gesetzt ist (fehlende Abhängigkeiten
fallen sonst still weg), und schreibt `linux.config`.

## Disk-Layout

`output/images/disk.img` (GPT), gemeinsam für x86_64 (UEFI) und Raspberry Pi 4/5:

| Partition  | Inhalt                                   |
|------------|------------------------------------------|
| `boot`     | FAT32/EFI: GRUB, `grub.cfg`, `grubenv` (A/B-Auswahl), Kernel pro Slot in `a/` und `b/` |
| `rootfs-a` | System A, squashfs, read-only            |
| `rootfs-b` | System B für Updates, anfangs leer       |
| `state`    | ext4, unter `/state`: Host-Keys, Seed, später Konfiguration und Daten |

A/B: `grubenv` enthält `slot=A|B` (fest) und `try_slot` (einmaliger Probeboot, wird vor dem Start
gelöscht). Fehlt der Kernel eines Slots, startet GRUB den anderen.

Lässt sich `state` nicht einbinden, läuft das System mit tmpfs weiter (Status in `/run/state.json`).
`disk.img` behält den Zustand zwischen Boots; ein neuer Build setzt ihn zurück.

## Updates

Jeder Build erzeugt `output/images/update.tar`. Einspielen (System + Kernel in den inaktiven Slot):

```sh
ssh root@host arcadeos-update install - < output/images/update.tar
ssh root@host reboot                      # startet einmalig in den neuen Slot
ssh root@host arcadeos-update confirm     # innerhalb von 10 min, sonst zurück zum alten Slot
```

`arcadeos-update status` zeigt laufenden und festen Slot, Probeboot, Restzeit und Versionen als JSON;
`arcadeos-update rollback` macht den anderen Slot wieder fest. Hängt oder stürzt das neue System ab,
startet der Watchdog bzw. `panic=10` neu und es landet im alten Slot.

Update-Pakete müssen signiert sein (minisign). Dev-Builds signiert die Build-Maschine automatisch;
für echte Geräte (Release-Build) signierst du mit deinem eigenen Schlüssel – siehe
[keys/README.md](keys/README.md) und `tools/sign-update.sh`.

## Konfiguration

```sh
ssh root@host arcadeos-config set hostname=pi-wohnzimmer
ssh root@host arcadeos-config set net.mode=static net.address=192.168.1.50/24 net.gateway=192.168.1.1 net.dns=1.1.1.1
ssh root@host arcadeos-config add-key "ssh-ed25519 AAAA... laptop"
ssh root@host arcadeos-config confirm     # über eine neue Verbindung, sonst nach 120 s zurück
```

Jede Änderung ist sofort aktiv und wird ohne `confirm` automatisch zurückgenommen – auch wenn die
Verbindung abreißt oder das Gerät neu startet. Die SSH-Keys aus dem Image (Recovery-Keys) gelten immer.

## Agenten-Zugang (MCP)

`agentd` ist ein MCP-Server über SSH. Für Claude Code (`.mcp.json`):

```json
{"mcpServers": {"arcadeos": {"command": "ssh",
  "args": ["-i", "~/.ssh/<key>", "root@<geraet>", "agentd"]}}}
```

Werkzeuge: `system_status`, `logs` (Audit-Log/Syslog), `update_confirm`, `update_rollback`,
`config_set`, `config_confirm`, `config_rollback`. Jede Änderung landet im Audit-Log
(`/state/log/audit.log`).

## Tests

```sh
tests/qemu-state.sh      # UEFI/GRUB, A/B-Probeboot, read-only Rootfs, Persistenz, Fallback
tests/qemu-update.sh     # A/B-Updates: Signaturen, kaputte Pakete, Frist, confirm, Watchdog, rollback
tests/qemu-config.sh     # Konfiguration: confirm, Frist, Neustart, falsche IP, SSH-Keys
tests/qemu-agentd.sh     # MCP-Server über SSH
tests/qemu-net.sh        # Netztuning (BBR, fq, notsent_lowat) und NTP
```

## Aufbau

- `br-external/` – Buildroot-External-Tree: Defconfig, Kernel-Config, Disk-Layout (genimage),
  Rootfs-Overlay, Post-Build/Post-Image-Skripte
- `tests/` – Tests in QEMU (`lib.sh`: gemeinsame Hilfsfunktionen)
- `tools/` – `sign-update.sh` (Update-Paket signieren)
- `meshd/` – Mesh-Dienst in Rust (Arcade-Mesh-Protokoll, [docs/mesh-protocol.md](docs/mesh-protocol.md));
  bisher Link-Schicht und `meshd probe host:port …` (Verbindungstest)
- `docs/` – Spezifikationen
- `keys/` – `authorized_keys` für root (nicht im Repo)
- `run.sh` – QEMU-Start mit UEFI/OVMF (KVM, falls vorhanden, sonst Emulation)
