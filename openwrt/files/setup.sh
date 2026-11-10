# Package install step shared by the apk and ipk packages: the settings
# directory, writable by the service user only, with the settings file
# (created from the example). Runs before default_postinst, which enables
# and starts the service, so the file must exist by then.
mkdir -p "${IPKG_INSTROOT}/etc/internet-radio-proxifier"
[ -f "${IPKG_INSTROOT}/etc/internet-radio-proxifier/settings.ini" ] || cp "${IPKG_INSTROOT}/usr/share/internet-radio-proxifier/settings.example.ini" "${IPKG_INSTROOT}/etc/internet-radio-proxifier/settings.ini"
chown -R nobody:nogroup "${IPKG_INSTROOT}/etc/internet-radio-proxifier" 2>/dev/null || chown -R 65534:65534 "${IPKG_INSTROOT}/etc/internet-radio-proxifier"
chmod 700 "${IPKG_INSTROOT}/etc/internet-radio-proxifier"
chmod 600 "${IPKG_INSTROOT}/etc/internet-radio-proxifier/settings.ini"
