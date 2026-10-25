# Gemeinsame Funktionen fuer ArcadeOS-Skripte (wird per "." eingebunden).

# Geraetename (z.B. vda4) der Partition mit diesem GPT/MBR-Partitionsnamen
find_part() {
	for u in /sys/class/block/*/uevent; do
		grep -qx "PARTNAME=$1" "$u" 2>/dev/null || continue
		sed -n 's/^DEVNAME=//p' "$u"
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
