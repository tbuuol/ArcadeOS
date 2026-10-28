# ArcadeOS

Schlankes Linux für einen KI-Betreiber: Linux-Kernel, musl, busybox und dropbear, gebaut mit
[Buildroot](https://buildroot.org). Kein GUI, Zugang nur per SSH-Key. Ziele und Phasen stehen in
[PLAN.md](PLAN.md).

## Bauen

Benötigt (Ubuntu/Debian): `build-essential unzip qemu-system-x86 ovmf`

Für die Tests empfohlen: eigenes QEMU 10.2 (Ubuntus QEMU 8.2.2 fällt ohne KVM bei ~3 % der Neustarts aus,
siehe PLAN.md). `run.sh` nimmt es automatisch, wenn es gebaut ist:

```sh
make -C buildroot-2026.02.3 O=$PWD/output-tools BR2_EXTERNAL=$PWD/br-external tools_qemu_defconfig
make -C output-tools host-qemu       # einmalig, ca. 40 min
```

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

`run.sh` kann auch echte PC-Hardware nachstellen:

```sh
FIRMWARE=bios ./run.sh                     # alter PC ohne UEFI (SeaBIOS) statt OVMF
DISK_BUS=usb ./run.sh                      # Platte als USB-Stick (auch: uas, ahci, nvme, scsi, virtio)
NIC=e1000e ./run.sh                        # Netzwerkkarte (auch: e1000, igb, vmxnet3, usb, virtio-net-pci)
```

## Echte Hardware (USB-Stick)

Ein Image für alle x86_64-PCs, Laptops und Mini-PCs – mit UEFI und mit altem BIOS:

1. `output/images/disk.img` mit **balenaEtcher**, **Rufus** (DD-Modus) oder `dd` auf den Stick schreiben.
2. Eigenen SSH-Public-Key als `authorized_keys` (oder `authorized_keys.txt`) in die FAT-Partition
   des Sticks legen (unter Windows als Laufwerk „BOOT“ sichtbar, siehe `LIESMICH.txt` dort).
3. Vom Stick starten (UEFI: Secure Boot aus). Der Bildschirm zeigt IP, SSH-Befehl und den
   Fingerabdruck des Host-Keys – beim ersten `ssh root@<IP>` vergleichen.

Eingebaut: USB (auch UAS), SATA (HDD/SSD), NVMe, SD/eMMC; Netzwerk per Kabel: Intel
(e1000/e1000e/igb/igc), Realtek r8169/r8125 (mit Firmware), Broadcom tg3, Atheros, USB-LAN-Adapter;
VMs: virtio, VirtualBox, VMware, Hyper-V. Bildschirm: UEFI-Grafik (VGA/DVI/HDMI/DisplayPort, ohne
Grafiktreiber) bzw. VGA-Textmodus beim BIOS-Start. Noch nicht: WLAN, Raspberry Pi (eigenes Image).
DHCP läuft auf jeder Kabel-Netzwerkkarte.

## Kernel

Eigene, schlanke Config: `make tinyconfig` plus
[linux-fragment.config](br-external/board/arcadeos/linux-fragment.config) (PC-Treiber für Platten,
Netzwerk und Bildschirm, virtio, serielle Konsole, ext4, IPv4/IPv6, keine Module). Nach einer Änderung am Fragment neu erzeugen:

```sh
tar xf buildroot-2026.02.3/dl/linux/linux-6.18.7.tar.xz -C /tmp
PATH=$PWD/output/host/bin:$PATH br-external/board/arcadeos/gen-linux-config.sh /tmp/linux-6.18.7
```

Das Skript prüft, dass jede Option aus dem Fragment wirklich gesetzt ist (fehlende Abhängigkeiten
fallen sonst still weg), und schreibt `linux.config`.

## Disk-Layout

`output/images/disk.img` (GPT, auf 1 MB ausgerichtet), x86_64 mit UEFI und BIOS, später Raspberry Pi 4/5:

| Partition  | Inhalt                                   |
|------------|------------------------------------------|
| (Sektor 0) | GRUB-Startcode für BIOS-PCs              |
| `bios-boot`| GRUB-Kern für BIOS-PCs (1 MB, ohne Dateisystem) |
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

## Mesh

Im Image läuft `meshd` und verbindet sich mit den Live-Arcades (nur ausgehend, keine Chain-Ankündigung).

```sh
ssh root@host meshd status                       # Verbindungen, Antwortzeiten (JSON)
ssh root@host tail -f /state/mesh/debug.log      # Debug-Log, eine JSON-Zeile pro Ereignis
```

Startliste anpassen: `/state/mesh/seeds` (eine Adresse pro Zeile, `host:port` oder `wss://…`).
Abschalten: `touch /state/mesh/disabled`. Dauerhaft laufende VM auf diesem Server: `tools/live-vm.sh`.

## Tests

```sh
tests/qemu-state.sh      # UEFI/GRUB, A/B-Probeboot, read-only Rootfs, Persistenz, Fallback
tests/qemu-update.sh     # A/B-Updates: Signaturen, kaputte Pakete, Frist, confirm, Watchdog, rollback
tests/qemu-config.sh     # Konfiguration: confirm, Frist, Neustart, falsche IP, SSH-Keys
tests/qemu-agentd.sh     # MCP-Server über SSH
tests/qemu-net.sh        # Netztuning (BBR, fq, notsent_lowat) und NTP
tests/qemu-mesh.sh       # meshd: Live-Verbindungen, Key bleibt, Neustart nach Absturz (braucht Internet)
tests/qemu-hw.sh         # echte Hardware: UEFI/BIOS × USB/UAS/SATA/NVMe/SCSI × Netzwerkkarten,
                         # Statusseite + Bildschirmfotos, SSH-Key vom Stick
tests/reboot-soak.sh     # Neustart-Dauerlauf mit Diagnose (QMP-Abfrage bei Hänger)
```

## Aufbau

- `br-external/` – Buildroot-External-Tree: Defconfig, Kernel-Config, Disk-Layout (genimage),
  Rootfs-Overlay, Post-Build/Post-Image-Skripte
- `tests/` – Tests in QEMU (`lib.sh`: gemeinsame Hilfsfunktionen)
- `tools/` – `sign-update.sh` (Update-Paket signieren), `live-vm.sh` (dauerhafte Live-VM)
- `meshd/` – Mesh-Dienst in Rust (Arcade-Mesh-Protokoll, [docs/mesh-protocol.md](docs/mesh-protocol.md)):
  `meshd run` (Dauerbetrieb, im Image als Dienst `S60meshd`), `meshd status`, `meshd probe host:port|wss://… …`
- `docs/` – Spezifikationen
- `keys/` – `authorized_keys` für root (nicht im Repo)
- `run.sh` – QEMU-Start mit UEFI/OVMF (KVM, falls vorhanden, sonst Emulation); `DEBUG_DIR=…` für Diagnose
