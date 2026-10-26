# Arcade-Mesh-Protokoll (Version 2) – Spezifikation für ArcadeOS

ArcadeOS spricht das Mesh-Protokoll der bestehenden Arcades (DogecoinArcade), damit ArcadeOS-Instanzen
und Live-Arcades ein gemeinsames Netz bilden. Die Arcade-Software selbst ist **nicht** Teil von
ArcadeOS; diese Beschreibung ist aus dem öffentlichen Arcade-Code abgelesen (Stand 2026-10-26,
`arcade/mesh/link.py`, `node.py`, `service.py`, `announce.py`) und gegen Live-Arcades geprüft.

## Netz

- Netzname: `arcade-main` (die Arcades benennen ihr Netz nach ihrer Ledger-Chain, Pepecoin-Mainnet).
  Knoten mit anderem Netznamen verbinden sich nicht.
- Standardport: TCP **8421**. Alternativ WebSocket (`wss://…/mesh`) für Knoten hinter nicht
  öffnenbaren Routern; innen laufen dieselben Rahmen.
- Identität eines Knotens = sein **Ed25519-Mesh-Key** (32 Byte, hex = Knoten-ID). Die Adresse ist nur,
  wo er gerade erreichbar ist.

## WebSocket-Transport

Für Knoten hinter nicht öffnenbaren Routern (Tunnel, z.B. Cloudflare): `wss://host/mesh`.
- HTTP/1.1-Upgrade nach RFC 6455 (`Sec-WebSocket-Version: 13`), Client maskiert, binäre Nachrichten
- Die Nachrichten bilden einen **Bytestrom**: jede trägt die nächsten Bytes der normalen Mesh-Rahmen
  (inkl. 4-Byte-Länge); der Empfänger setzt sie aneinander. Die Arcades schicken pro Rahmen eine Nachricht.
- Darin läuft derselbe Handshake; der Tunnel sieht nur Chiffretext.
- Nachrichten höchstens 128 KB; WebSocket-Pings werden beantwortet.

## Eine Verbindung pro Knoten

Ein Knoten hält zu jedem anderen Knoten (gleiche Knoten-ID) höchstens **eine** Verbindung; eine zweite
wird nach dem Handshake geschlossen – auch wenn sie über eine andere Adresse kommt (z.B. TCP und Tunnel
zum selben Knoten).

## Rahmen auf der Leitung

Jeder Rahmen: 4 Byte Länge (big endian), dann der Körper. Körper höchstens **65.536 Byte**.

## Handshake

1. Beide Seiten senden sofort einen Rahmen `HELLO`, unverschlüsselt:

       "ARCM" | Version (1 Byte, = 2) | Länge Netzname (1 Byte) | Netzname (UTF-8) | X25519-Public-Key (32 Byte)

   Ein frischer X25519-Schlüssel pro Verbindung. Falscher Netzname oder Version → Verbindung beenden.
2. `shared = X25519(eigener ephemeral privat, fremder ephemeral public)`
3. Rollen: wer gewählt hat, ist **Dialer** („D“), die Gegenseite **Listener** („L“). Bei gleichzeitigem
   Öffnen (IPv6-„Meeting“) ist der Knoten mit der kleineren Knoten-ID der Dialer.
4. `transcript = BLAKE2b-256(HELLO_dialer ‖ HELLO_listener)` (jeweils der kodierte HELLO-Inhalt ohne
   Längenpräfix)
5. Schlüssel: `k = BLAKE2b-256(shared ‖ transcript, personal = Label, auf 16 Byte mit 0x00 aufgefüllt)`
   - `k_out` mit Label `arcm-dialer`, `k_in` mit Label `arcm-listener`
   - Dialer sendet mit `k_out` und empfängt mit `k_in`, Listener umgekehrt.
6. Ab jetzt ist alles verschlüsselt (siehe unten). Beide senden `AUTH`:

       {"t":"auth","key":<eigener Mesh-Key hex>,"sig":<hex>,"listen":<Port oder null>}

   `sig = Ed25519-Signatur über "arcade-mesh-auth-v1" ‖ Rolle ("D"|"L") ‖ transcript`.
   Die empfangene Signatur wird mit der **Gegenrolle** geprüft. Eigener Key = Verbindung zu sich selbst
   → beenden. Zeitlimit für Handshake-Schritte: 10 s.

## Verschlüsselung

XChaCha20-Poly1305 (IETF), ohne zusätzliche Daten. Nonce = Rahmenzähler pro Richtung, ab 0, als
24-Byte-Big-Endian-Zahl. Ein Rahmen, der nicht authentifiziert, beendet die Verbindung.

## Inhalt eines Rahmens

- **JSON-Rahmen:** 2 Byte Länge des Kopfes (big endian) ‖ JSON-Kopf (kompakt) ‖ optional binärer Anhang
- **Binärer Raum-Rahmen (`pub`):** beginnt mit `FF FF 01`:

       FF FF | 01 | origin 32 | sig 64 | q u64 | hops u8 | kind u8 (0 msg,1 join,2 leave,3 here)
       | room u8+Bytes | member u8+Bytes | cred u16+kanonisches JSON (0 = keins)
       | d u16 Anzahl (FFFF = keins) + je 4 Byte | Nutzlast

  Ergibt dasselbe Dict wie die JSON-Form; Signaturen werden über das Dict geprüft (siehe unten).

## Nachrichtenarten (Feld `t`)

| `t` | Zweck | Status in ArcadeOS |
|---|---|---|
| `ping` `{n}` / `pong` `{n}` | Lebenszeichen, RTT; ping alle 15 s, tot nach 45 s | Link-Schicht |
| `addrs` `{a:[{h,p,id}]}` | Adressen, die eine Seite tatsächlich erreicht hat | Link-Schicht |
| `sub` | signierte Liste der Räume eines Knotens (8-Byte-Fingerabdrücke), geflutet | Räume |
| `pub` | Raumnachricht (msg/join/leave/here), signiert, entlang der SUB-Wege | Räume |
| `dialback`, `dialback-said` | Erreichbarkeit der eigenen Adresse prüfen | später |
| `cs` `cr` `ix` `op` `st` `dx` `iw` `lm` … | Erweiterungen der Arcade-Software (Core-Tunnel, geteilte Cores, Index-Vergleich, Betreiber, STUN, …) | ignorieren |

Unbekannte Arten werden **ignoriert**, nicht als Fehler behandelt.

## Signaturen über Nachrichten (`sub`, `pub`)

- Signiert wird das kanonische JSON aller Felder **außer** `sig`, `hops`, `d`
  (Python: `json.dumps(h, separators=(",",":"), sort_keys=True)`, also ASCII mit `\uXXXX`-Escapes),
  bei `pub` gefolgt von der Nutzlast.
- `o` = Ursprungsknoten (Key hex), `q` = Mikrosekunden-Zeitstempel als Sequenz; Nachrichten mit
  mehr als **60 s** Abweichung von der eigenen Uhr werden verworfen → **Uhrzeit muss stimmen (NTP)**.
- Doppelte (`o:q` schon gesehen) werden verworfen; höchstens 16 Hops.

## Grenzen (wie die Arcades)

Nutzlast 512 Byte, 5 Nachrichten/s je Mitglied (Burst 10), Weiterleiter erlauben das Doppelte.
6 ausgehende Verbindungen als Ziel, bis 32 eingehende.

## Gemessen (2026-10-26, gegen Live-Arcades)

| Ziel | Netz (ICMP) | Mesh-Ping Median | p90 | p99 |
|---|---|---|---|---|
| lokale Arcade | – | 1,7 ms | 12 ms | 283 ms |
| 207.244.232.43 | 107 ms ±2 | 118 ms | 240 ms | 1.027 ms |
| 104.153.31.40 | – | 123 ms | 364 ms | 1.119 ms |
| wss://app.dogecoinarcade.com (Cloudflare) | – | 146 ms | 158 ms | 190 ms |

Die Ausreißer entstehen in den Arcade-Prozessen (Python-GIL, Mesh teilt sich den Prozess mit Web und
Index), nicht im Netz. Ein eigener, nativer Mesh-Dienst vermeidet sie auf ArcadeOS-Knoten.
