#!/bin/sh
# Richtet die Mainboard-Lüftersteuerung für indigo ein:
#  - lädt den Super-I/O-Treiber nct6775 (jetzt und bei jedem Boot)
#  - erlaubt der Gruppe des aufrufenden Nutzers das Schreiben der pwm-Dateien
# Einmalig ausführen:  sudo sh packaging/setup-fan-control.sh
set -e

if [ "$(id -u)" != 0 ]; then
    echo "bitte mit sudo ausführen" >&2
    exit 1
fi

TARGET_USER="${SUDO_USER:-$(logname)}"
TARGET_GROUP="$(id -gn "$TARGET_USER")"

echo "lade nct6775 ..."
modprobe nct6775
echo nct6775 > /etc/modules-load.d/indigo-fans.conf

echo "installiere udev-regel (schreibrecht für gruppe $TARGET_GROUP) ..."
cat > /etc/udev/rules.d/99-indigo-fans.rules <<EOF
ACTION=="add", SUBSYSTEM=="hwmon", RUN+="/bin/sh -c 'chgrp $TARGET_GROUP /sys%p/pwm* 2>/dev/null; chmod g+w /sys%p/pwm* 2>/dev/null; true'"
EOF
udevadm control --reload
udevadm trigger -s hwmon -c add

sleep 1
echo
echo "gefundene lüfter:"
found=0
for d in /sys/class/hwmon/hwmon*; do
    name="$(cat "$d/name" 2>/dev/null)"
    for f in "$d"/fan[0-9]_input; do
        [ -e "$f" ] || continue
        rpm="$(cat "$f" 2>/dev/null)"
        echo "  $name $(basename "$f" _input): ${rpm} rpm"
        found=1
    done
done

if [ "$found" = 0 ]; then
    # häufiger fall auf asus-boards: chip erkannt, aber acpi reserviert
    # den io-bereich -> kernel-parameter acpi_enforce_resources=lax nötig
    if dmesg | grep -q "SystemIO range.*conflicts with OpRegion"; then
        echo "  keine — acpi blockiert den zugriff auf den sensorchip."
        if grep -q "acpi_enforce_resources=lax" /etc/default/grub; then
            echo "  kernel-parameter ist bereits gesetzt: bitte neu starten."
        else
            echo "  trage acpi_enforce_resources=lax in /etc/default/grub ein ..."
            cp /etc/default/grub /etc/default/grub.indigo-backup
            sed -i 's/^GRUB_CMDLINE_LINUX_DEFAULT="\(.*\)"$/GRUB_CMDLINE_LINUX_DEFAULT="\1 acpi_enforce_resources=lax"/' /etc/default/grub
            update-grub
            echo
            echo "  fertig (backup: /etc/default/grub.indigo-backup)."
            echo "  bitte neu starten — danach erscheinen die lüfter automatisch."
        fi
    else
        echo "  keine — der chip wird von nct6775 evtl. nicht unterstützt"
    fi
fi
