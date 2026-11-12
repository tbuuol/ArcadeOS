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
2. Eigenen SSH-Public-Key als `authorized_keys.txt` (oder `authorized_keys`) ins Laufwerk
   **ARCADEOS** des Sticks legen – Windows, Mac und Linux zeigen es nach dem Schreiben sofort an
   (siehe `LIESMICH.txt` dort).
   Für WLAN zusätzlich `wifi.txt` mit `ssid=…`, `password=…`, `country=DE` (mehrere Netze: weitere
   Paare darunter).
3. Vom Stick starten (UEFI: Secure Boot aus). Der Bildschirm zeigt IP, SSH-Befehl und den
   Fingerabdruck des Host-Keys – beim ersten `ssh root@<IP>` vergleichen.

Beim ersten Start wächst die Datenpartition auf den ganzen Stick. Agenten (z.B. die Claude-App per
SSH) finden alles, was sie brauchen: Login-Shell bash, ein beschreibbares, bleibendes `/root`
(liegt auf `/state/root`), `stat`/`timeout`/`zstd` und einen sftp-Server (`scp` ohne `-O`).

### Auf die Platte installieren

Der Stick ist zum Installieren da: vom Stick gestartet ist das Gerät per SSH erreichbar, die
Installation läuft dann aus der Ferne. Die Zielplatte wird dabei **gelöscht**:

```sh
ssh root@<IP> arcadeos-install list                        # Platten (ohne den Stick), als JSON
ssh root@<IP> arcadeos-install to nvme0n1 --wipe nvme0n1   # Name zweimal = Bestätigung
```

Kopiert werden Startcode, Boot-Partition, System A und B, ARCADEOS (SSH-Keys, `wifi.txt`) und `/state`
(Host-Key, Konfiguration, Mesh-Identität) – das Gerät bleibt dasselbe. Die Datenpartition bekommt den
ganzen Rest der Platte (ext4). Unter UEFI legt der Installer den Starteintrag „ArcadeOS“ an die erste
Stelle. Danach Stick abziehen und neu starten. Steckt der Stick weiter, startet trotzdem jedes Medium
nur sein eigenes System und seinen eigenen Zustand (GRUB startet per Partitions-UUID der eigenen
Platte, `/state` wird nur auf der Startplatte gesucht).

Eingebaut: USB (auch UAS), SATA (HDD/SSD), NVMe, SD/eMMC; Netzwerk per Kabel: Intel
(e1000/e1000e/igb/igc), Realtek r8169/r8125 (mit Firmware), Broadcom tg3, Atheros, USB-LAN-Adapter;
VMs: virtio, VirtualBox, VMware, Hyper-V. Bildschirm: UEFI-Grafik (VGA/DVI/HDMI/DisplayPort, ohne
Grafiktreiber) bzw. VGA-Textmodus beim BIOS-Start. WLAN: Intel (7260 bis AX210, Firmware für 8000C/
8265/9xxx) und Qualcomm QCA9377/QCA6174 (ath10k), WPA2 und offene Netze (kein reines WPA3).
Noch nicht: Raspberry Pi (eigenes Image). DHCP läuft auf jeder Kabel-Netzwerkkarte und im WLAN.

## Kernel

Eigene, schlanke Config: `make tinyconfig` plus
[linux-fragment.config](br-external/board/arcadeos/linux-fragment.config) (PC-Treiber für Platten,
Netzwerk und Bildschirm, virtio, serielle Konsole, ext4, IPv4/IPv6). Fest eingebaut bis auf WLAN:
dessen Treiber laden ihre Firmware schon beim Erkennen der Karte, also vor dem Root-Dateisystem –
darum Module, geladen von `S05modules` über den modalias der Geräte. Nach einer Änderung am Fragment neu erzeugen:

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
| `arcadeos` | FAT, Laufwerk „ARCADEOS“ (Basic Data, 16 MB): `authorized_keys.txt`, `wifi.txt`, `LIESMICH.txt` |
| `state`    | ext4, unter `/state`: Host-Keys, Konfiguration, Daten, `/root`; auf dem Stick beim ersten Start bis ans Ende vergrößert |

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

**Selbst bestätigen:** Im Probeboot prüft `update-healthcheck` alle 15 s, ob das neue System gesund ist
(`/state` persistent, SSH lauscht, `meshd` läuft mit frischem Status und – mit Netz – mindestens einer
Verbindung). Dreimal hintereinander gesund → es ruft selbst `arcadeos-update confirm` auf (typisch nach
~50 s). Sonst greift die Frist wie oben. Nur noch von Hand bestätigen: `echo 0 > /state/update/autoconfirm`.

**Updates auf dem Gerät bauen** (ohne Buildroot, z.B. für einen Agenten auf dem Gerät): `tools/box-update.sh
<dateien-dir> <zusatz> --install` nimmt das System des laufenden Slots, legt die Dateien aus `<dateien-dir>`
darüber, packt es neu, signiert mit dem Geräte-Schlüssel `/state/update/keys/box.key` (minisign) und spielt es
in den inaktiven Slot. Der zugehörige Public Key gehört nach `keys/trusted/release/`, damit Images ihm vertrauen.
Neuer Kernel: `--kernel <bzImage> --modules <dir>` (Module aus `make modules_install INSTALL_MOD_PATH=<dir>`, ersetzen
`/lib/modules` komplett, damit sie zum Kernel passen). Mit dem Compiler der Build-Umgebung `GCC_PLUGINS` abschalten:
Buildroots Compiler kann keine, die Images sind ohne gebaut.
Braucht die Build-Umgebung unter `/state/build` (Alpine-chroot mit Rust und squashfs-tools).

## Konfiguration

```sh
ssh root@host arcadeos-config set hostname=pi-wohnzimmer
ssh root@host arcadeos-config set net.mode=static net.address=192.168.1.50/24 net.gateway=192.168.1.1 net.dns=1.1.1.1
ssh root@host arcadeos-config add-key "ssh-ed25519 AAAA... laptop"
ssh root@host arcadeos-config set "wifi.ssid=Mein WLAN" "wifi.psk=geheim123" wifi.country=DE
ssh root@host arcadeos-config confirm     # über eine neue Verbindung, sonst nach 120 s zurück
```

Jede Änderung ist sofort aktiv und wird ohne `confirm` automatisch zurückgenommen – auch wenn die
Verbindung abreißt oder das Gerät neu startet. Die SSH-Keys aus dem Image (Recovery-Keys) gelten immer.
Das WLAN-Passwort steht nur in `/state/config/system.conf` (nur root); `show`, Audit-Log und die
wpa_supplicant-Datei enthalten es nicht. `wifi.ssid=` (leer) schaltet das WLAN aus der Konfiguration ab.

## Weitere Platten

Zusätzliche Platten (Daten, Chains, Indizes) bindet `S12mounts` beim Start unter `/media/<name>` ein – immer über
die UUID, mit `nosuid,nodev,noatime`, den Dateisystemtyp erkennt `mount` selbst. Eine Zeile je Platte in
`/state/config/mounts`:

```
# <UUID>                              <name>  [rw|ro]
33c7cd30-27c1-438a-b93d-f83989eaeeb6  hdd     rw
```

UUID finden: `blkid`. Sofort einbinden ohne Neustart: `/etc/init.d/S12mounts restart`. Fehlt eine Platte, startet das
System normal weiter; Grund im Audit-Log (`disk-mount-failed`), Zustand in `/run/mounts.json`. Formatiert wird nie.
Die Monitoring-Seite zeigt eingebundene Platten mit Belegung an.

## Monitoring-Seite

Jedes Gerät zeigt unter `http://<IP>/` live, was los ist (Syfy-UI, Themes Atlantis/OASIS):
CPU, RAM, Belegung von System- und Datenpartition, Laufwerke, Temperatur, Akku/Netzteil,
Netzwerk (Kabel/WLAN-Signal, Verkehr), Dienste, Mesh-Verbindungen und das Audit-Log.

- **Nur lesen:** `monitor-collect` (root) schreibt alle 2 s `/run/monitor/status.json`; busybox
  `httpd` läuft als `nobody` und liefert nur `index.html(.gz)` und diese Datei aus – kein CGI, kein
  POST, keine Anmeldung, keine Steuerung.
- **Öffentlich lesbar:** ohne Passwort. Deshalb enthält sie keine Passwörter/Keys, keinen WLAN-Namen
  (auch im Protokoll maskiert), keine Adressen von SSH-Clients oder Mesh-Gegenstellen.
- **Von außen:** Portfreigabe im Router auf Port 80 des Geräts (wie für SSH).
- **Port:** `arcadeos-config set monitor.port=8080`, `monitor.port=0` schaltet sie ab.
- Quelle der Seite: `br-external/board/arcadeos/overlay/usr/share/arcadeos/monitor/index.html`,
  erzeugt aus dem Repo [Syfy-UI](https://github.com/tbuuol/Syfy-UI) mit `tools/build-monitor-page.py`.

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

Im Image läuft `meshd` und verbindet sich mit den Live-Arcades (keine Chain-Ankündigung). Er nimmt auch
Verbindungen an: TCP **8421** auf IPv6 und IPv4, bis 32 eingehende (zusätzlich zu den 6 ausgehenden), und nennt den
Port im Handshake – so können andere Knoten ihn ohne Tunnel erreichen. Dafür am Router Port 8421 (TCP) an das
Gerät freigeben (IPv4: Portweiterleitung; IPv6: Freigabe für das Gerät, z.B. Fritzbox „Portfreigaben“).
Anderer Port: Zahl in `/state/mesh/listen`, `0` = nur ausgehend; danach `/etc/init.d/S60meshd restart`.
Ob die Freigabe wirkt, prüfen die Arcades selbst: `meshd` bittet jede direkt erreichte Arcade um einen Rückruf
(dialback); `meshd status` zeigt `reachable: true` und die Adresse, unter der sie das Gerät sehen (`seen_as`).

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
tests/qemu-wifi.sh       # WLAN mit simulierten Funkkarten: wifi.txt, arcadeos-config, Rollback
tests/qemu-monitor.sh    # Monitoring-Seite: nur lesen, Werte, nichts Geheimes, monitor.port
tests/qemu-install.sh    # Installer: Stick -> NVMe, Start von der Platte (UEFI/BIOS), Stick + Platte
tests/qemu-claude.sh     # Plug and Play fuer Agenten: Key aus ARCADEOS, state waechst, bash, /root,
                         # Claude-App-Server und Claude Code (musl) vom Stick und von der Platte
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
