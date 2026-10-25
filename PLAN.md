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

### Phase 0 – Boot → SSH  *(fertig, 2026-10-25)*
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
- Offen: serielle Konsole ohne Passwort (nur für QEMU ok), busybox-`nslookup` stolpert
  über Kommentar in `/etc/resolv.conf`, Host-Keys bei jedem neuen Image neu (Phase 1: State-Partition)

### Phase 1 – AI-OS-Basis
- Root-Dateisystem read-only (squashfs), Zustand auf eigener Partition
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

### Phase 3 – Wallet-Anbindung
- PSBT-Brücke zum Gerät des Users (QR / App / Hardware-Wallet)
- Optionaler Signier-Dienst für Service-Wallets (standardmäßig aus)

### Später
- Echte Hardware (Raspberry Pi, x86 Bare Metal), Kernel weiter abspecken
