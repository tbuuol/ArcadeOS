# ArcadeOS – schlankes OS für einen KI-Betreiber

Linux-Kernel + eigenes, minimales Userland (Buildroot). Kein GUI, Zugang nur per SSH-Key.

## Grundsätze

- **Self-Custody:** Private Keys der User liegen nie auf dem System. Das OS kennt nur
  öffentliche Daten (xpub/Adressen) und baut unsignierte PSBTs; signiert wird auf dem Gerät des Users.
- **Service-Wallets** nur für Dienste, die selbst ein Wallet brauchen: eigene Keys pro Dienst,
  in einem separaten Signier-Dienst, mit fest eingebauten Limits, vom User aufgefüllt.
- **Nie aussperren:** Änderungen als Transaktion mit „commit confirmed“ und Auto-Rollback.
- **Strukturierte Schnittstellen:** Zustand, Logs und Fortschritt als JSON (später MCP über SSH).
- **Audit-Log:** jede Aktion des Agenten append-only protokolliert.

## Phasen

### Phase 0 – Boot → SSH  *(abgeschlossen, 2026-10-25)*
- Buildroot 2026.02.3 LTS, x86_64, musl, busybox, dropbear, Kernel 6.18.7
- Login nur per SSH-Key (`~/.ssh/aios_ed25519`), Passwort-Login über SSH aus (geprüft)
- Läuft in QEMU (`./run.sh`, SSH auf localhost:2222)
- Ergebnis: bzImage 6,6 MB, Rootfs belegt 5,2 MB, 11 MB RAM nach dem Boot,
  SSH nach 14 s (ohne KVM), Build ca. 30 min
- Kernel abgespeckt: tinyconfig + `board/arcadeos/linux-fragment.config` (nur virtio, serielle
  Konsole, ext4, IPv4/6, keine Module/USB/VT/Input). Vergleich mit gleichem Rootfs, ohne KVM,
  Median aus 5 Boots:

  |                     | QEMU-Standard | ArcadeOS |
  |---------------------|---------------|----------|
  | bzImage             | 6,6 MB        | 3,5 MB   |
  | RAM belegt / frei   | 11 / 235 MB   | 9 / 240 MB |
  | Kernel-Build        | 354 s         | 83 s     |
  | Login-Prompt        | 8,8 s         | 8,9 s    |
  | SSH bereit          | 10,2 s        | 11,1 s   |

  Boot-Zeit unverändert (Streuung auf dem geteilten VPS ±2 s); unter Emulation dominiert nicht die
  Kernelgröße. Der erste Boot eines neuen Images braucht ~10 s länger (dropbear erzeugt Host-Keys).
- Konsole gesperrt: root hat kein nutzbares Passwort (`*`), Login nur per SSH-Key (getestet:
  leeres/beliebiges Passwort an der Konsole → zurück zum Prompt, keine Shell)
- DNS: udhcpc-Skript schreibt pro Interface nach `/run/udhcpc/<if>` und baut `resolv.conf`
  ohne Kommentare zusammen → busybox-`nslookup` funktioniert, Renew ohne Duplikate
- Übertragen nach Phase 1: Host-Keys bei jedem neuen Image neu (→ Zustands-Partition)

### Phase 1 – AI-OS-Basis  *(abgeschlossen, 2026-10-26)*
- ✅ **Root-Dateisystem read-only, Zustand auf eigener Partition** (2026-10-25)
  - `rootfs-a` (squashfs/zstd, 2,0 MB), `rootfs-b` (leer, für A/B), `state` (ext4)
  - Kernel bootet `root=PARTLABEL=rootfs-a ro` – unabhängig vom Gerätenamen
  - `S00state` sucht die Partition `state` per Partitionsname (`/sys/class/block`, ohne udev),
    mountet sie unter `/state`; schlägt das fehl → tmpfs, System bleibt per SSH erreichbar
  - Status maschinenlesbar in `/run/state.json` (`persistent`/`ephemeral` + Grund)
  - Persistent: dropbear-Host-Keys (`/etc/dropbear` → `/state/dropbear`), Zufalls-Seed
  - Test: `tests/qemu-state.sh` (14 Prüfungen: ro, Persistenz über Neustart, Fallback)
  - Noch offen: kein `fsck` beim Start (ext4-Journal fängt unsauberes Abschalten meist ab),
    `state` wächst noch nicht auf die volle Plattengröße (→ Installer)
- ✅ **Bootloader + UEFI** (2026-10-25) – Layout gemeinsam für x86 und Pi 4/5:
  - `disk.img` (GPT): `boot` (FAT32/ESP) · `rootfs-a` 512M · `rootfs-b` 512M · `state` (ext4)
  - `boot` enthält `EFI/BOOT/bootx64.efi` (GRUB 2.12, 600 KB), `grub.cfg`, `grubenv` und den
    Kernel pro Slot in `a/` bzw. `b/`; später zusätzlich Pi-Firmware, `config.txt`, `tryboot.txt`
  - Kernel als EFI-Programm (EFI-Stub), `run.sh` bootet über OVMF wie ein echter PC
  - **A/B im Bootloader:** `grubenv` hat `slot` (fest) und `try_slot` (einmaliger Probeboot,
    wird vor dem Start gelöscht); fehlt der Kernel eines Slots, startet GRUB den anderen.
    Gleiches Prinzip auf dem Pi mit `tryboot.txt` + `os_prefix=b/` (nicht getestet, keine Hardware)
  - Tests: 19 Prüfungen, u.a. Probeboot auf leeren Slot B → Fallback auf A, `try_slot` verbraucht
  - Boot bis SSH unter Emulation jetzt ~16 s (UEFI-Firmware kostet ~5 s)
- ✅ **A/B-Updates** (2026-10-25)
  - `arcadeos-update status|install|confirm|rollback`, Ausgabe als JSON
  - Paket `update.tar` (manifest + rootfs.squashfs + bzImage) entsteht bei jedem Build; Version
    = Datum + Git-Stand. Install auch gestreamt: `ssh root@host arcadeos-update install - < update.tar`
  - Install prüft Prüfsummen, Architektur, Platz; schreibt nur den inaktiven Slot, liest zurück,
    setzt den einmaligen Probeboot. Verweigert, solange ein Probeboot unbestätigt läuft
  - Probeboot: Frist (`/state/update/confirm_timeout`, Standard 600 s), danach Neustart → alter Slot.
    `confirm` über neue SSH-Verbindung macht den Slot fest (beweist nebenbei, dass SSH geht)
  - Watchdog (`S01watchdog`, 30 s; Treiber i6300esb/iTCO/sp5100) und `panic=10`: Hänger oder
    Absturz im Probeboot → Neustart → alter Slot
  - Tests: `tests/qemu-update.sh`, 35 Prüfungen (kaputte Pakete, Frist, confirm, Watchdog, rollback)
  - Noch offen: Pi-Weg (tryboot) ungetestet
- ✅ **Signierte Update-Pakete** (2026-10-26) – minisign (Ed25519, libsodium)
  - Prüfung vor jedem Schreibzugriff gegen `/etc/arcadeos/update-keys/*.pub`; Signatur deckt das
    Manifest und damit (über die Prüfsummen) System und Kernel ab
  - Zwei Schlüsselarten: **Release-Key des Users** (`keys/trusted/release/`, privat nur beim User,
    signiert mit `tools/sign-update.sh`) und **Build-Key** dieser Maschine (`keys/trusted/dev/`,
    privat in `~/.config/arcadeos/dev.key`, signiert jeden Dev-Build automatisch)
  - `BR2_ARCADEOS_RELEASE=y`: Gerät vertraut nur Release-Keys; ohne Release-Key bricht der Build ab.
    Dev-Build (QEMU): zusätzlich Build-Key
- ✅ **Zentrale Konfiguration + „commit confirmed“** (2026-10-26)
  - `/state/config/system.conf` (hostname, net.mode dhcp|static, net.address, net.gateway, net.dns,
    config.confirm_timeout) und `/state/config/authorized_keys`
  - `arcadeos-config show|set|add-key|remove-key|confirm|rollback`; jede Änderung sofort aktiv, ohne
    `confirm` nach der Frist (Standard 120 s) zurück – auch nach Neustart während der Frist
  - Immer zuerst sichere Vorgaben (DHCP, Recovery-SSH-Keys aus dem Image), Konfiguration darüber:
    eine kaputte Konfiguration sperrt nie aus
- ✅ **agentd + Audit-Log** (2026-10-26)
  - `agentd`: MCP-Server (Protokoll 2025-06-18) über SSH-stdio, in Lua 5.4. Werkzeuge:
    system_status, logs, update_confirm, update_rollback, config_set, config_confirm, config_rollback
  - Audit-Log `/state/log/audit.log`: eine JSON-Zeile je Änderung (Update, Konfiguration, Bestätigung,
    Rollback, abgelehnte Pakete) mit Zeit, Slot und SSH-Gegenstelle
- Tests: `tests/qemu-{state,update,config,agentd}.sh`
- Größe: Rootfs 2,4 MB (squashfs), Kernel 3,6 MB

### Netztuning + Zeit  *(fertig, 2026-10-26)*
- BBR-Staukontrolle, fq als Warteschlange, `tcp_notsent_lowat=16384`: kleine Nachrichten warten nicht
  hinter Megabytes im Sendepuffer; BBR hält den Puffer im Router/Modem kurz (langsamer Upload zu Hause)
- Kernel zusätzlich mit fq_codel und cake (für spätere Bandbreitenbegrenzung)
- Zeit: busybox-ntpd (Pool-Server, `arcadeos-config set ntp.servers=…`), Status `/run/ntp.json`
- agentd `system_status` zeigt Uhrzeit/NTP und Netz; Test `tests/qemu-net.sh` (14 Prüfungen)

### Phase 2a – Mesh  *(in Arbeit)*
**Entscheidung (tbuuol, 2026-10-26):** Arcade-Software und ArcadeOS bleiben getrennt, nichts wird
zusammengeführt. ArcadeOS bekommt einen **eigenen Mesh-Dienst `meshd`**, der das Mesh-Protokoll der
Arcades spricht (Spezifikation: [docs/mesh-protocol.md](docs/mesh-protocol.md)) und so mit den
Live-Arcades ein gemeinsames Netz bildet.

Warum ein eigener Dienst – gemessen 2026-10-26 gegen Live-Arcades:
- Protokoll effizient: 0,17 ms CPU je Weiterleitung (Martins Code), nachbauen lohnt dafür nicht
- aber Ausreißer: Netz 107 ms ±2, Mesh-Ping p90 219–364 ms, p99 bis ~1 s – im Arcade-Prozess
  (Mesh teilt sich den Python-GIL mit Web/Index), nicht im Netz; mit eigenem Rust-Client bestätigt
- `meshd` in **Rust**: speichersicher (Daten aus dem Internet), kein GIL, keine GC-Pausen, ~0,7 MB

Meilensteine:
1. ✅ Link-Schicht: Rahmen, HELLO, Handshake (X25519, BLAKE2b, Ed25519), XChaCha20-Poly1305,
   ping/pong, addrs; `meshd probe` verbindet sich mit allen erreichbaren Live-Arcades
2. WebSocket-Transport (`wss://…/mesh`; zwei Live-Arcades sind nur so erreichbar)
3. Dauerbetrieb: Peer-Verwaltung (6 ausgehend, bis 32 eingehend), Adressbuch auf `/state`,
   Mesh-Key auf `/state`, ping/Timeout, Weiterleitung von `sub`/`pub` (kanonisches JSON wie Python)
4. Räume für lokale Anwendungen (Schnittstelle über Unix-Socket), Grenzen wie die Arcades
5. Peers aus Chain-Ankündigungen lesen (braucht Zugriff auf Pepecoin-Daten), eigene Ankündigung
6. IPv6-„Meeting“, Dialback, optional Portfreigabe am Router (NAT-PMP)
7. Buildroot-Paket, Init-Skript, Watchdog, agentd-Werkzeuge (Peers, Räume, RTT), Tests in QEMU

Nebenbefund: auf diesem Server liefen 18 verwaiste Worker-Prozesse der Arcade (475 MB) von früheren
Neustarts – gehört nicht zu ArcadeOS, Hinweis an den Betreiber.

### Phase 2b – Node + Indexer
- Node(s) als Dienst (Arcade-Chain, optional Kotia)
- Index-Bus: verteilt Blöcke an Module, zentrales Reorg-Handling
- Modul **utxo-db**: Adresse/Scripthash → UTXOs + Historie; Watch-only = Abfrage per xpub;
  Grundlage für PSBTs; optional Electrum-kompatibel
- Modul **asset-db**: Arcade-Protokolle aus OP_RETURN (Spiele, Versionen, Registry, Assets,
  Kotia-Chunks), Zusammensetzen + Hash-Prüfung
- Module einzeln an-/abschaltbar und neu indexierbar, Fortschritt als JSON
- Auswahl per Konfiguration beim Aufsetzen (`nodes:`, `indexers:`, `watch:`)
- **Speicherort pro Node/Indexer wählbar** (wichtig für den Pi mit kleiner SD-Karte):
  ```yaml
  nodes:
    arcade:
      enabled: true
      storage:
        device: /dev/sda1     # oder label:/uuid: – gemountet wird immer per UUID
        path: /data/arcade    # Zielpfad auf der Platte
        format: false         # nur nach ausdrücklicher Bestätigung
  ```
  - Standard ohne Angabe: Zustands-Partition des Systems
  - Hookup zeigt erkannte Platten/Partitionen mit Größe, freiem Platz und Dateisystem,
    warnt, wenn der Platz für Chain + Indizes absehbar nicht reicht
  - Formatieren nie automatisch, nur nach Bestätigung, mit Anzeige was gelöscht würde
  - **Schutz gegen volllaufende SD-Karte:** Dienst startet nur, wenn die Platte wirklich
    gemountet ist (sonst würde er in den leeren Mountpoint auf der SD schreiben); fehlt sie,
    bleibt der Dienst gestoppt und meldet das als Status
  - Umziehen auf eine andere Platte später möglich (Dienst stoppen, kopieren, umhängen)

### Phase 3 – Wallet-Anbindung
- PSBT-Brücke zum Gerät des Users (QR / App / Hardware-Wallet)
- Optionaler Signier-Dienst für Service-Wallets (standardmäßig aus)

### Hardware-Profile und Installer  *(gehört eng zu Phase 1: Bootloader + A/B-Layout)*
Ziel: Test auf Raspberry Pi, Betrieb auch auf beliebigen x86_64-Geräten.

| Profil          | Arch    | Boot                         | Kernel                                   |
|-----------------|---------|------------------------------|------------------------------------------|
| `qemu_x86_64`   | x86_64  | QEMU direkt (Entwicklung)    | tinyconfig + virtio (wie jetzt)          |
| `pc_x86_64`     | x86_64  | UEFI (+ BIOS-Fallback)       | USB, SATA, NVMe, gängige NICs + Firmware, Notfall-Bildschirm; seltene Treiber als Module |
| `rpi_arm64`     | aarch64 | Pi-Firmware (Pi 4/5, USB/SD) | Pi-Kernel + Device Tree                  |
| `vps_x86_64`    | x86_64  | UEFI/BIOS von Platte         | wie QEMU + Bootloader                    |

- **Universal-Stick:** eine FAT-Bootpartition kann gleichzeitig die Pi-Firmware (`config.txt`,
  arm64-Kernel) und `EFI/BOOT/BOOTX64.EFI` (x86-Kernel) enthalten – derselbe Stick bootet auf
  Pi und PC, jeder zieht sich seinen Teil. (Zu prüfen: Pi 5 / Pi 4 USB-Boot, UEFI mit MBR.)
- Auf der FAT-Partition liegt `authorized_keys` – der User legt seinen Key dort ab, nicht im Image.
- Gebootet: DHCP, SSH, meldet sich als `arcadeos.local` (mDNS) und zeigt die IP auf dem Bildschirm.
- **Installer erkennt die Plattform selbst** und wählt das passende Build:
  Architektur (`uname -m`), Pi-Modell (`/proc/device-tree/model`), x86-Hersteller/Modell (DMI),
  UEFI vs. BIOS, CPU-Features, Netzwerkkarten/Platten (PCI/USB-IDs) → Profil + ggf. Module/Firmware.
  Zeigt das Ergebnis vor der Installation an; Profil manuell überschreibbar.
- Installation läuft per SSH (durch den Agenten): Zielplatte wählen (Bestätigung), Layout
  Boot + A + B + Zustand anlegen, mit „commit confirmed“ bis der Rechner sauber von der Platte bootet.

### Später
- Kernel weiter abspecken, weitere Boards
