#!/bin/sh
# Build an OpenWrt apk (v3) package from a prebuilt static binary, the same
# way the OpenWrt SDK's include/package-pack.mk does (without compiling in
# the SDK). Only the SDK's host tools (apk, fakeroot) are used.
#
#   SDK=/path/to/openwrt-sdk-25.x openwrt/make-apk.sh BINARY ARCH OUTPUT.apk
#
#   BINARY  e.g. target/aarch64-unknown-linux-musl/release/radio
#   ARCH    OpenWrt package architecture, e.g. aarch64_cortex-a53, x86_64
#
# Env: SDK (required), VERSION (default: VERSION file + "-r1"), SIGN_KEY
# (default: $SDK/private-key.pem if it exists; unsigned otherwise, install
# with apk add --allow-untrusted).
set -eu
umask 022  # package dirs 0755, files 0644, like OpenWrt's own

[ $# -eq 3 ] || { sed -n '2,13p' "$0" >&2; exit 1; }
BIN=$1 ARCH=$2 OUT=$3
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
: "${SDK:?set SDK to an OpenWrt 25.x SDK directory}"
HOSTBIN=$SDK/staging_dir/host/bin
VERSION=${VERSION:-$(tr -d ' \r\n' < "$ROOT/VERSION")-r1}
SIGN_KEY=${SIGN_KEY:-$SDK/private-key.pem}
PKG=radio
case $OUT in /*) ;; *) OUT=$PWD/$OUT ;; esac

[ -f "$BIN" ] || { echo "missing $BIN" >&2; exit 1; }
[ -x "$HOSTBIN/apk" ] || { echo "apk not found in $HOSTBIN (set SDK=...)" >&2; exit 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
IDIR=$WORK/data
ADIR=$WORK/scripts
mkdir -p "$IDIR/usr/bin" "$IDIR/etc/init.d" "$IDIR/etc/config" \
	"$IDIR/usr/share/radio" "$IDIR/lib/upgrade/keep.d" "$IDIR/lib/apk/packages" "$ADIR"

install -m 0755 "$BIN"                        "$IDIR/usr/bin/radio"
install -m 0755 "$HERE/files/radio.init"      "$IDIR/etc/init.d/radio"
install -m 0644 "$HERE/files/radio.config"    "$IDIR/etc/config/radio"
install -m 0644 "$ROOT/settings.example.ini"  "$IDIR/usr/share/radio/settings.example.ini"
# The station list is created at install time (not shipped), so keep it
# across sysupgrade explicitly. /etc/config/radio is kept as a conffile.
echo /etc/radio/ > "$IDIR/lib/upgrade/keep.d/radio"

SETUP=$(cat "$HERE/files/setup.sh")

cat > "$ADIR/post-install" <<EOF
#!/bin/sh
[ "\${IPKG_NO_SCRIPT}" = "1" ] && exit 0
[ -s \${IPKG_INSTROOT}/lib/functions.sh ] || exit 0
. \${IPKG_INSTROOT}/lib/functions.sh
export root="\${IPKG_INSTROOT}"
export pkgname="$PKG"
$SETUP
add_group_and_user
default_postinst
EOF
{
	echo '#!/bin/sh'
	echo 'export PKG_UPGRADE=1'
	sed '/^\s*#!/d' "$ADIR/post-install"
	# default_postinst only "start"s the service, which procd ignores when it
	# is already running with the same command, so the new binary would not
	# be used until the next reboot.
	echo '[ -z "${IPKG_INSTROOT}" ] && /etc/init.d/radio restart'
	echo 'exit 0'
} > "$ADIR/post-upgrade"
cat > "$ADIR/pre-deinstall" <<EOF
#!/bin/sh
[ -s \${IPKG_INSTROOT}/lib/functions.sh ] || exit 0
. \${IPKG_INSTROOT}/lib/functions.sh
export root="\${IPKG_INSTROOT}"
export pkgname="$PKG"
default_prerm
EOF

(cd "$IDIR" && find . -type f,l -printf "/%P\n" | sort > "$WORK/list")
mv "$WORK/list" "$IDIR/lib/apk/packages/$PKG.list"
echo /etc/config/radio > "$IDIR/lib/apk/packages/$PKG.conffiles"
echo "/etc/config/radio $(sha256sum "$IDIR/etc/config/radio" | cut -d' ' -f1)" \
	> "$IDIR/lib/apk/packages/$PKG.conffiles_static"

rm -f "$OUT"
"$HOSTBIN/fakeroot" -l "$SDK/staging_dir/host/lib/libfakeroot.so" -f "$HOSTBIN/faked" \
	"$HOSTBIN/apk" mkpkg \
	--info "name:$PKG" \
	--info "version:$VERSION" \
	--info "description:Web radio player that streams internet radio stations through the router, with a station manager and an optional SOCKS5/HTTP proxy per station" \
	--info "arch:$ARCH" \
	--info "license:LGPL-2.1" \
	--info "origin:radio" \
	--info "url:https://github.com/stavultras/internet-radio-proxifier" \
	--info "maintainer:stavultras" \
	--info "depends:ca-bundle" \
	--script "post-install:$ADIR/post-install" \
	--script "post-upgrade:$ADIR/post-upgrade" \
	--script "pre-deinstall:$ADIR/pre-deinstall" \
	--files "$IDIR" \
	--output "$OUT"

if [ -f "$SIGN_KEY" ]; then
	"$HOSTBIN/apk" adbsign --allow-untrusted --sign-key "$SIGN_KEY" "$OUT"
	echo "signed with $SIGN_KEY"
fi
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
