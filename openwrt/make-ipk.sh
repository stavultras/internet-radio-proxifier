#!/bin/sh
# Build an OpenWrt ipk package (opkg: OpenWrt 24.10 and older) from a
# prebuilt static binary, the same way the OpenWrt 24.10 SDK's
# include/package-pack.mk does, with its scripts/ipkg-build.
#
#   IPKG_BUILD=/path/to/ipkg-build openwrt/make-ipk.sh BINARY ARCH OUTPUT.ipk
#
#   BINARY      e.g. target/aarch64-unknown-linux-musl/release/internet-radio-proxifier
#   ARCH        OpenWrt package architecture, e.g. aarch64_cortex-a53, x86_64
#   IPKG_BUILD  scripts/ipkg-build of an OpenWrt 24.10 SDK or source tree
#
# Env: VERSION (default: VERSION file + "-r1")
set -eu
umask 022

[ $# -eq 3 ] || { sed -n '2,12p' "$0" >&2; exit 1; }
BIN=$1 ARCH=$2 OUT=$3
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
VERSION=${VERSION:-$(tr -d ' \r\n' < "$ROOT/VERSION")-r1}
PKG=internet-radio-proxifier
case $OUT in /*) ;; *) OUT=$PWD/$OUT ;; esac
: "${IPKG_BUILD:?set IPKG_BUILD to scripts/ipkg-build of OpenWrt 24.10}"
[ -f "$BIN" ] || { echo "missing $BIN" >&2; exit 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
IDIR=$WORK/$PKG
mkdir -p "$IDIR/usr/bin" "$IDIR/etc/init.d" "$IDIR/etc/config" \
	"$IDIR/usr/share/$PKG" "$IDIR/lib/upgrade/keep.d" "$IDIR/CONTROL" "$WORK/out"

install -m 0755 "$BIN"                        "$IDIR/usr/bin/$PKG"
install -m 0755 "$HERE/files/$PKG.init"      "$IDIR/etc/init.d/$PKG"
install -m 0644 "$HERE/files/$PKG.config"    "$IDIR/etc/config/$PKG"
install -m 0644 "$ROOT/settings.example.ini"  "$IDIR/usr/share/$PKG/settings.example.ini"
# The settings file is created at install time (not shipped), so keep it
# across sysupgrade explicitly. /etc/config/internet-radio-proxifier is kept as a conffile.
echo "/etc/$PKG/" > "$IDIR/lib/upgrade/keep.d/$PKG"

cd "$IDIR/CONTROL"
cat > control <<EOF
Package: $PKG
Version: $VERSION
Depends: ca-bundle
Source: https://github.com/stavultras/internet-radio-proxifier
SourceName: $PKG
License: LGPL-2.1
LicenseFiles: LICENSE
Section: multimedia
URL: https://github.com/stavultras/internet-radio-proxifier
Maintainer: stavultras
Architecture: $ARCH
Installed-Size: $(du -sb --exclude=CONTROL "$IDIR" | cut -f1)
Description:  Web radio player that streams internet radio stations through the router, with a station manager and an optional SOCKS5/HTTP proxy per station
EOF
echo "/etc/config/$PKG" > conffiles
# postinst and prerm exactly as the SDK writes them; the package's own
# step is postinst-pkg, which default_postinst runs before it enables and
# starts the service.
cat > postinst <<'EOF'
#!/bin/sh
[ "${IPKG_NO_SCRIPT}" = "1" ] && exit 0
[ -s ${IPKG_INSTROOT}/lib/functions.sh ] || exit 0
. ${IPKG_INSTROOT}/lib/functions.sh
default_postinst $0 $@
EOF
cat > prerm <<'EOF'
#!/bin/sh
[ -s ${IPKG_INSTROOT}/lib/functions.sh ] || exit 0
. ${IPKG_INSTROOT}/lib/functions.sh
default_prerm $0 $@
EOF
{
	cat "$HERE/files/setup.sh"
	# On an upgrade the old binary may still be running, and "start" in
	# default_postinst leaves a running service alone: restart it.
	echo '[ -z "${IPKG_INSTROOT}" ] && pidof internet-radio-proxifier >/dev/null && /etc/init.d/internet-radio-proxifier restart'
	echo 'true'
} > postinst-pkg
chmod 0644 control conffiles postinst-pkg
chmod 0755 postinst prerm
cd "$WORK"

# tar with --owner: no fakeroot needed for root-owned files. File times:
# the last commit (reproducible); LC_ALL=C so that tar can parse the date.
EPOCH=$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || date +%s)
LC_ALL=C PKG_SOURCE_DATE_EPOCH=$EPOCH TAR="tar --owner=0 --group=0" \
	bash "$IPKG_BUILD" "$IDIR" "$WORK/out" >/dev/null
rm -f "$OUT"
mv "$WORK/out/${PKG}_${VERSION}_${ARCH}.ipk" "$OUT"
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
