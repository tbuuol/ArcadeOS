#!/bin/sh
# Plug and Play fuer Agenten per SSH (wie die Claude-App): vom Stick, dann installiert.
#   - Stick groesser als das Image (8 GB): state waechst beim ersten Start auf den ganzen Stick
#   - eigener Key nur in ARCADEOS/authorized_keys.txt (wie ein User unter Windows ihn ablegt)
#   - Login-Shell bash, /root beschreibbar und bleibend, stat/timeout/zstd, scp ohne -O (sftp)
#   - der Server der Claude-App und Claude Code (linux-x64-musl) laufen
#   - nach Neustart noch da; nach arcadeos-install auf die NVMe-Platte ebenso (ohne Stick)
# Aufruf: tests/qemu-claude.sh   Dauer ca. 20 min (ohne KVM).
# -cpu max: Claude Code (Bun) braucht AVX/AVX2 wie jeder PC der letzten 10 Jahre, das QEMU-Standardmodell hat sie nicht.
# Braucht: ~/.cache/arcadeos-test/claude-<version>-musl (offizieller Build, Pruefsumme aus manifest.json)
#          ~/.claude/remote/srv/*/server (Server der Claude-App, statisch)
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

CLAUDE=$(ls -t "$HOME"/.cache/arcadeos-test/claude-*-musl 2>/dev/null | head -1)
SRV=$(ls -t "$HOME"/.claude/remote/srv/*/server 2>/dev/null | head -1)
[ -x "$CLAUDE" ] || { echo "Claude Code (musl) fehlt in ~/.cache/arcadeos-test"; exit 2; }
[ -x "$SRV" ] || { echo "Server der Claude-App fehlt (~/.claude/remote/srv)"; exit 2; }

cp "$IMG/disk.img" $SP/stick.img
truncate -s 8G $SP/stick.img
truncate -s 8G $SP/target.img
# Key wie unter Windows: Datei mit BOM und CRLF ins Laufwerk ARCADEOS
ssh-keygen -q -t ed25519 -N "" -C stick-user -f $SP/userkey
printf '\357\273\277%s\r\n' "$(cat $SP/userkey.pub)" > $SP/authorized_keys.txt
OFF=$(/sbin/sfdisk -d $SP/stick.img 2>/dev/null | awk -F'[=,]' '/name="arcadeos"/{gsub(/ /,"",$2); print $2*512}')
[ -n "$OFF" ] && ok "Partition arcadeos im Image" || { bad "keine Partition arcadeos"; finish; exit 1; }
/sbin/sfdisk -d $SP/stick.img 2>/dev/null | grep 'name="arcadeos"' | grep -qi 'EBD0A0A2-B9E5-4433-87C0-68B6B72699C7' \
    && ok "arcadeos ist Basic Data (Windows gibt einen Laufwerksbuchstaben)" || bad "arcadeos hat nicht den Typ Basic Data"
"$ROOT/output/host/bin/minfo" -i "$SP/stick.img@@$OFF" :: 2>/dev/null | grep -q 'ARCADEOS' \
    && ok "Laufwerksname ARCADEOS" || bad "Laufwerksname: $("$ROOT/output/host/bin/minfo" -i "$SP/stick.img@@$OFF" :: 2>&1 | grep -i label)"
"$ROOT/output/host/bin/mtype" -i "$SP/stick.img@@$OFF" ::LIESMICH.txt | grep -q authorized_keys \
    && ok "LIESMICH.txt auf ARCADEOS" || bad "LIESMICH.txt fehlt auf ARCADEOS"
"$ROOT/output/host/bin/mcopy" -i "$SP/stick.img@@$OFF" $SP/authorized_keys.txt ::authorized_keys.txt \
    && ok "Key auf ARCADEOS kopiert" || bad "mcopy fehlgeschlagen"

US="timeout 60 ssh -i $SP/userkey $SSHOPTS -o IdentitiesOnly=yes root@127.0.0.1"
USL="timeout 900 ssh -i $SP/userkey $SSHOPTS -o IdentitiesOnly=yes root@127.0.0.1"
SCP="timeout 900 scp -P 2222 -i $SP/userkey -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o BatchMode=yes"
TARGET="-drive file=$SP/target.img,if=none,id=tg,format=raw -device nvme,drive=tg,serial=target1"

boot() {  # <name> <umgebung...> -- <qemu-argumente...>
    name=$1; shift
    env_args=""
    while [ "$1" != -- ]; do env_args="$env_args $1"; shift; done; shift
    env DEBUG_DIR=$SP/debug VARS=$SP/vars.fd SSH_PORT=2222 MEM=1536 $env_args "$ROOT/run.sh" -cpu max \
        -serial file:$SP/serial-$name.log -monitor none -display none "$@" >/dev/null 2>&1 &
    VM_PID=$!
}
stop() { kill "$VM_PID" 2>/dev/null; wait "$VM_PID" 2>/dev/null; VM_PID=""; }

# Was die Claude-App von einem Rechner erwartet (bei jedem Start pruefen)
agent_checks() {  # <wo>
    check "$($US 'echo $SHELL')" "/bin/bash" "$1: Login-Shell bash"
    $US 'grep -qx /bin/bash /etc/shells' && ok "$1: bash in /etc/shells" || bad "$1: bash fehlt in /etc/shells"
    $US 'bash -c "[[ 1 == 1 ]] && echo \$BASH_VERSION"' | grep -q '^5\.' && ok "$1: bash laeuft" || bad "$1: bash laeuft nicht"
    $US 'mkdir -p ~/.claude/remote && touch ~/.claude/remote/probe && rm ~/.claude/remote/probe' \
        && ok "$1: ~/.claude beschreibbar" || bad "$1: ~/.claude nicht beschreibbar"
    $US 'stat -c %s /etc/passwd' | grep -qE '^[0-9]+$' && ok "$1: stat" || bad "$1: stat fehlt"
    $US 'timeout 1 true' && ok "$1: timeout" || bad "$1: timeout fehlt"
    $US 'echo abc | zstd -q | zstd -dq' | grep -qx abc && ok "$1: zstd" || bad "$1: zstd fehlt"
    $US 'for c in sha256sum base64 mktemp tar gzip find xargs pgrep pkill setsid nohup flock uname readlink realpath env; do command -v $c >/dev/null || echo $c; done' > $SP/missing.txt
    [ -s $SP/missing.txt ] && bad "$1: es fehlen $(tr '\n' ' ' < $SP/missing.txt)" || ok "$1: uebliche Befehle vorhanden"
    $US 'test -e /lib/libc.musl-x86_64.so.1' && ok "$1: musl unter Alpine-Namen" || bad "$1: libc.musl-x86_64.so.1 fehlt"
}

echo "== 1. Vom Stick (8 GB, USB), daneben eine leere NVMe-Platte"
boot stick DISK=$SP/stick.img DISK_BUS=usb -- $TARGET
wait_ssh && ok "SSH vom Stick" || { bad "kein SSH"; tail -30 $SP/serial-stick.log; finish; exit 1; }
[ "$($US echo drin)" = drin ] && ok "Login mit dem Key aus ARCADEOS" || bad "Key aus ARCADEOS abgelehnt"
SZ=$($US 'df -m /state' | awk 'NR==2{print $2}')
[ "${SZ:-0}" -gt 6000 ] && ok "state auf den ganzen Stick vergroessert ($SZ MB)" || bad "state nur ${SZ:-?} MB"
$US 'grep -q "\"action\":\"state-grow\"" /state/log/audit.log' && ok "Audit: state-grow" || bad "kein Audit state-grow"
$US 'sgdisk -v /dev/sda' | grep -q 'No problems found' && ok "GPT des Sticks fehlerfrei" || bad "GPT: $($US 'sgdisk -v /dev/sda')"
check "$($US 'grep " /root " /proc/mounts' | awk '{print $1}')" "/dev/sda6" "/root liegt auf state"
agent_checks Stick

# Wie die Claude-App: erst ihr Server, dann Claude Code; hier per scp, also ueber sftp (ohne -O)
$US 'mkdir -p ~/.claude/remote/srv/test ~/.claude/remote/ccd-cli'
$SCP "$SRV" root@127.0.0.1:.claude/remote/srv/test/server && ok "scp ohne -O (sftp-server)" || bad "scp fehlgeschlagen"
$US '~/.claude/remote/srv/test/server --version' | grep -q '^claude-ssh' && ok "Server der Claude-App laeuft" || bad "Server der Claude-App laeuft nicht"
T0=$(date +%s)
$SCP "$CLAUDE" root@127.0.0.1:.claude/remote/ccd-cli/claude && ok "Claude Code hochgeladen ($(( $(date +%s) - T0 )) s, $(( $(stat -c %s "$CLAUDE") / 1048576 )) MB)" \
    || bad "Claude Code hochladen fehlgeschlagen"
check "$($US 'sha256sum ~/.claude/remote/ccd-cli/claude' | cut -d' ' -f1)" "$(sha256sum "$CLAUDE" | cut -d' ' -f1)" "Claude Code vollstaendig"
V=$($USL '~/.claude/remote/ccd-cli/claude --version 2>&1')
case "$V" in *"Claude Code"*) ok "Claude Code laeuft auf dem Stick ($V)" ;; *) bad "Claude Code: $V" ;; esac

echo "== 2. Neustart vom Stick: alles noch da"
B=$($US cat /proc/sys/kernel/random/boot_id)
$US reboot >/dev/null 2>&1
T0=$(date +%s)
while [ $(( $(date +%s) - T0 )) -lt 600 ]; do n=$($US cat /proc/sys/kernel/random/boot_id 2>/dev/null); [ -n "$n" ] && [ "$n" != "$B" ] && break; sleep 2; done
[ -n "$n" ] && [ "$n" != "$B" ] && ok "wieder da nach Neustart" || bad "kein Neustart"
check "$($US 'df -m /state' | awk 'NR==2{print ($2>6000)}')" "1" "state bleibt gross"
check "$($US 'grep -c state-grow /state/log/audit.log')" "1" "nur einmal vergroessert"
$US 'test -x ~/.claude/remote/ccd-cli/claude' && ok "Claude Code noch da" || bad "Claude Code weg nach Neustart"
agent_checks "Stick nach Neustart"

echo "== 3. Vom Stick auf die Platte installieren (ueber den User-Key, wie ein Agent)"
T0=$(date +%s)
$USL 'arcadeos-install to nvme0n1 --wipe nvme0n1' > $SP/install.log 2>&1
tail -1 $SP/install.log | grep -q '"pct":100' && ok "Installation ($(( $(date +%s) - T0 )) s)" || bad "Installation: $(tail -3 $SP/install.log)"
check "$($US 'sgdisk -p /dev/nvme0n1' | awk '/^ +[0-9]+ /{print $1":"$NF}' | tr '\n' ' ')" "1:bios-boot 2:boot 3:rootfs-a 4:rootfs-b 5:arcadeos 6:state " "Partitionen auf der Platte"
stop

echo "== 4. Nur die Platte (Stick ab)"
boot disk DISK=$SP/target.img DISK_BUS=nvme --
wait_ssh && ok "SSH von der Platte" || { bad "Platte startet nicht"; tail -30 $SP/serial-disk.log; finish; exit 1; }
[ "$($US echo drin)" = drin ] && ok "Login mit dem Key aus ARCADEOS (mitkopiert)" || bad "Key auf der Platte abgelehnt"
check "$($US 'grep " /root " /proc/mounts' | awk '{print $1}')" "/dev/nvme0n1p6" "/root liegt auf state der Platte"
$US 'grep -q state-grow /state/log/audit.log' && check "$($US 'grep -c state-grow /state/log/audit.log')" "1" "Platte nicht noch einmal vergroessert" || true
agent_checks Platte
V=$($USL '~/.claude/remote/ccd-cli/claude --version 2>&1')
case "$V" in *"Claude Code"*) ok "Claude Code laeuft auf der Platte ($V)" ;; *) bad "Claude Code auf der Platte: $V" ;; esac
stop

finish
