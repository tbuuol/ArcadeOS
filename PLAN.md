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

### Phase 1 – AI-OS-Basis  *(in Arbeit)*
- ✅ **Root-Dateisystem read-only, Zustand auf eigener Partition** (2026-10-25)
  - `disk.img` (GPT): `rootfs-a` (squashfs/zstd, 2,0 MB), `rootfs-b` (leer, für A/B), `state` (ext4)
  - Kernel bootet `root=PARTLABEL=rootfs-a ro` – unabhängig vom Gerätenamen
  - `S00state` sucht die Partition `state` per Partitionsname (`/sys/class/block`, ohne udev),
    mountet sie unter `/state`; schlägt das fehl → tmpfs, System bleibt per SSH erreichbar
  - Status maschinenlesbar in `/run/state.json` (`persistent`/`ephemeral` + Grund)
  - Persistent: dropbear-Host-Keys (`/etc/dropbear` → `/state/dropbear`), Zufalls-Seed
  - Test: `tests/qemu-state.sh` (14 Prüfungen: ro, Persistenz über Neustart, Fallback)
  - Noch offen: kein `fsck` beim Start (ext4-Journal fängt unsauberes Abschalten meist ab),
    `state` wächst noch nicht auf die volle Plattengröße (→ Installer)
- A/B-Images, „commit confirmed“ mit Auto-Rollback
- `agentd`: Systemzustand als JSON / MCP-Server über SSH
- Audit-Log

### Phase 2 – Node + Indexer
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
