# ArcadeOS

Schlankes Linux für einen KI-Betreiber: Linux-Kernel, musl, busybox und dropbear, gebaut mit
[Buildroot](https://buildroot.org). Kein GUI, Zugang nur per SSH-Key. Ziele und Phasen stehen in
[PLAN.md](PLAN.md).

## Bauen

Benötigt (Ubuntu/Debian): `build-essential unzip qemu-system-x86`

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

## Aufbau

- `br-external/` – Buildroot-External-Tree: Defconfig, Kernel-Config, Rootfs-Overlay, Post-Build-Skript
- `keys/` – `authorized_keys` für root (nicht im Repo)
- `run.sh` – QEMU-Start (KVM, falls vorhanden, sonst Emulation)
