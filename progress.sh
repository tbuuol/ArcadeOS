#!/bin/sh
# Zeigt den Build-Fortschritt: fertige Pakete / alle Pakete, Laufzeit, geschaetzte Restzeit.
D="$(dirname "$0")"
TOTAL=$(wc -l < "$D/targets.txt")
DONE=0
for t in $(cat "$D/targets.txt"); do
    ls "$D"/output/build/$t-*/.stamp_*installed "$D"/output/build/$t/.stamp_*installed 2>/dev/null | grep -q . && DONE=$((DONE+1))
done
EL=$(( $(date +%s) - $(cat "$D/build.start") ))
PCT=$(( DONE * 100 / TOTAL ))
if [ "$DONE" -gt 0 ]; then REST=$(( EL * (TOTAL - DONE) / DONE )); else REST=0; fi
CUR=$(grep -ao '>>> [^ ]* [^ ]* [A-Z][a-z]*' "$D/build.log" | tail -1)
printf '%d%% (%d/%d Pakete) · Laufzeit %dm%02ds · Rest ca. %dm · %s\n' \
    "$PCT" "$DONE" "$TOTAL" $((EL/60)) $((EL%60)) $((REST/60)) "$CUR"
grep -q '^EXIT' "$D/build.log" && grep '^EXIT' "$D/build.log"
