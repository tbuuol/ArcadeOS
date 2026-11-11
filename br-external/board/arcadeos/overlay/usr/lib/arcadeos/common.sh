# Gemeinsame Funktionen fuer ArcadeOS-Skripte (wird per "." eingebunden).

# Platte, von der das laufende System stammt (z.B. sda, nvme0n1): Elternteil der Root-Partition
boot_disk() {
	_bd_mm=$(awk '$5 == "/" { print $3; exit }' /proc/self/mountinfo)
	_bd_p=$(readlink -f "/sys/dev/block/$_bd_mm" 2>/dev/null) || return 1
	[ -e "$_bd_p/partition" ] || return 1
	basename "$(dirname "$_bd_p")"
}

# Geraetename (z.B. vda4) der Partition mit diesem GPT/MBR-Partitionsnamen. Nur auf der Startplatte:
# ein eingesteckter ArcadeOS-Stick neben der installierten Platte hat dieselben Namen.
find_part() {
	# (Variablen mit _fp_: das Skript wird eingebunden, keine Namen des Aufrufers ueberschreiben)
	_fp_d=$(boot_disk)
	if [ -n "$_fp_d" ]; then set -- "$1" /sys/block/"$_fp_d"/"$_fp_d"*/uevent; else set -- "$1" /sys/class/block/*/uevent; fi
	_fp_n=$1; shift
	for _fp_u in "$@"; do
		grep -qx "PARTNAME=$_fp_n" "$_fp_u" 2>/dev/null || continue
		sed -n 's/^DEVNAME=//p' "$_fp_u"
		return 0
	done
	return 1
}

# Slot, aus dem gerade gebootet wurde (A|B), laut Kernel-Kommandozeile
running_slot() {
	sed -n 's/.*arcadeos\.slot=\([AB]\).*/\1/p' /proc/cmdline
}

other_slot() {
	[ "$1" = A ] && echo B || echo A
}

# Sekunden seit dem Boot (unabhaengig von der Uhrzeit)
uptime_s() {
	cut -d. -f1 /proc/uptime
}

# Audit-Log: eine JSON-Zeile pro Aktion, nur anhaengen (/state/log/audit.log)
# audit <aktion> <detail>
audit() {
	[ -d /state/log ] || return 0
	who=${SSH_CONNECTION%% *}
	printf '{"t":"%s","uptime":%s,"slot":"%s","who":"%s","action":"%s","detail":"%s"}\n' \
		"$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(uptime_s)" "$(running_slot)" "${who:-local}" \
		"$(printf %s "$1" | tr -d '"\\\n')" "$(printf %s "$2" | tr -d '"\\\n')" >> /state/log/audit.log
}
