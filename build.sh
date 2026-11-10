#!/bin/sh
# Build release packages into dist/:
#
#   ./build.sh [TARGET...]      default: all
#
#   linux-x86_64     internet-radio-proxifier-<V>-linux-x86_64.tar.gz    (static, musl)
#   linux-aarch64    internet-radio-proxifier-<V>-linux-aarch64.tar.gz   (static, musl)
#   windows-x86_64   internet-radio-proxifier-<V>-windows-x86_64.zip
#   openwrt          internet-radio-proxifier-<V>-openwrt-<arch>.apk (OpenWrt
#                    25.x) and .ipk (OpenWrt 24.10) for aarch64, x86_64,
#                    mipsel_24kc and mips_24kc (needs SDK=..., see
#                    openwrt/make-apk.sh; the ipk packages are made with
#                    OpenWrt 24.10's scripts/ipkg-build, downloaded once)
#
# Needs: rustup targets x86_64-unknown-linux-musl, aarch64-unknown-linux-musl,
# x86_64-pc-windows-gnu; gcc-aarch64-linux-gnu and gcc-mingw-w64-x86-64 for
# the C parts of ring; zip, curl, zstd.
#
# MIPS: Rust has no prebuilt standard library for it, so it is built with
# $NIGHTLY (+ rust-src, -Z build-std) and linked statically with OpenWrt's
# own musl toolchain, downloaded once into $TOOLCHAINS.
set -eu
cd "$(dirname "$0")"
ROOT=$PWD
V=$(tr -d ' \r\n' < VERSION)
NAME=internet-radio-proxifier
DIST=$ROOT/dist
# OpenWrt package architectures; the aarch64 binary runs on all of them.
OPENWRT_AARCH64="aarch64_cortex-a53 aarch64_cortex-a72 aarch64_cortex-a76 aarch64_generic"
OPENWRT_X86_64="x86_64"
OPENWRT_RELEASE=${OPENWRT_RELEASE:-25.12.5}
OPENWRT24_RELEASE=${OPENWRT24_RELEASE:-24.10.8}
NIGHTLY=${NIGHTLY:-nightly-2026-11-09}
TOOLCHAINS=${TOOLCHAINS:-$HOME/.cache/openwrt-toolchains}

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER:-aarch64-linux-gnu-gcc}
export CC_aarch64_unknown_linux_musl=${CC_aarch64_unknown_linux_musl:-aarch64-linux-gnu-gcc}
export AR_aarch64_unknown_linux_musl=${AR_aarch64_unknown_linux_musl:-aarch64-linux-gnu-ar}
export CC_x86_64_unknown_linux_musl=${CC_x86_64_unknown_linux_musl:-gcc}

bin() { # rust target -> path of the built binary
	case $1 in
		*windows*) echo "target/$1/release/internet-radio-proxifier.exe" ;;
		*) echo "target/$1/release/internet-radio-proxifier" ;;
	esac
}

build() { # rust target
	echo "== cargo build --target $1"
	cargo build --release --locked --target "$1"
}

stage() { # dir name -> fresh staging dir with the common files
	d=$DIST/stage/$1
	rm -rf "$d" && mkdir -p "$d"
	cp README.md LICENSE settings.example.ini "$d/"
	echo "$d"
}

linux() { # arch rust-target
	build "$2"
	d=$(stage "$NAME-$V")
	install -m 0755 "$(bin "$2")" "$d/internet-radio-proxifier"
	tar -C "$DIST/stage" --owner=0 --group=0 -czf "$DIST/$NAME-$V-linux-$1.tar.gz" "$NAME-$V"
	rm -rf "$DIST/stage"
	echo "built $DIST/$NAME-$V-linux-$1.tar.gz"
}

windows() {
	build x86_64-pc-windows-gnu
	d=$(stage "$NAME-$V")
	cp "$(bin x86_64-pc-windows-gnu)" "$d/internet-radio-proxifier.exe"
	# Double-click starter: settings.ini next to internet-radio-proxifier.exe (created with
	# example stations on the first run), player opened in the browser.
	printf '%s\r\n' '@echo off' 'cd /d "%~dp0"' \
		'start "" http://localhost:8123/' \
		'internet-radio-proxifier.exe --host :8123 --config settings.ini' 'pause' > "$d/start.cmd"
	(cd "$DIST/stage" && rm -f "$DIST/$NAME-$V-windows-x86_64.zip" && zip -qr9 "$DIST/$NAME-$V-windows-x86_64.zip" "$NAME-$V")
	rm -rf "$DIST/stage"
	echo "built $DIST/$NAME-$V-windows-x86_64.zip"
}

# OpenWrt toolchain for target/subtarget, downloaded on first use; prints
# its toolchain-* directory.
toolchain() {
	f=openwrt-toolchain-$OPENWRT_RELEASE-${1%/*}-${1#*/}_gcc-14.3.0_musl.Linux-x86_64
	if [ ! -d "$TOOLCHAINS/$f" ]; then
		mkdir -p "$TOOLCHAINS"
		echo "== downloading $f" >&2
		curl -sfL "https://downloads.openwrt.org/releases/$OPENWRT_RELEASE/targets/$1/$f.tar.zst" |
			tar --zstd -x -C "$TOOLCHAINS"
	fi
	echo "$TOOLCHAINS/$f"/toolchain-*
}

mips() { # rust target, OpenWrt target/subtarget
	tc=$(toolchain "$2")
	gcc=$(echo "$tc"/bin/*-openwrt-linux-musl-gcc)
	# Rust's static musl link asks for libunwind; gcc's libgcc_eh has it.
	mkdir -p "$tc/rust-unwind"
	ln -sf "$(echo "$tc"/lib/gcc/*/*/libgcc_eh.a)" "$tc/rust-unwind/libunwind.a"
	T=$(echo "$1" | tr a-z- A-Z_) t=$(echo "$1" | tr - _)
	echo "== cargo +$NIGHTLY build -Z build-std --target $1"
	env STAGING_DIR="$tc" \
		"CARGO_TARGET_${T}_LINKER=$gcc" "CC_$t=$gcc" "AR_$t=${gcc%gcc}ar" \
		"CARGO_TARGET_${T}_RUSTFLAGS=-C target-feature=+crt-static -C link-self-contained=no -L $tc/rust-unwind" \
		cargo "+$NIGHTLY" build --release --locked -Z build-std=std,panic_abort --target "$1"
}

# OpenWrt 24.10's ipk builder (a shell script, the same as in its SDK)
ipkg_build() {
	f=$TOOLCHAINS/ipkg-build-$OPENWRT24_RELEASE
	if [ ! -s "$f" ]; then
		mkdir -p "$TOOLCHAINS"
		curl -sfL "https://raw.githubusercontent.com/openwrt/openwrt/v$OPENWRT24_RELEASE/scripts/ipkg-build" -o "$f.tmp"
		mv "$f.tmp" "$f"
	fi
	echo "$f"
}

# apk (OpenWrt 25.x) and ipk (24.10) packages of one binary
package() { # binary, OpenWrt arch
	openwrt/make-apk.sh "$1" "$2" "$DIST/$NAME-$V-openwrt-$2.apk"
	IPKG_BUILD=$(ipkg_build) openwrt/make-ipk.sh "$1" "$2" "$DIST/$NAME-$V-openwrt-$2.ipk"
}

openwrt() {
	: "${SDK:?set SDK to an OpenWrt 25.x SDK directory (for its apk and fakeroot tools)}"
	for t in aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
		build $t
	done
	for a in $OPENWRT_AARCH64; do
		package "$(bin aarch64-unknown-linux-musl)" "$a"
	done
	for a in $OPENWRT_X86_64; do
		package "$(bin x86_64-unknown-linux-musl)" "$a"
	done
	mips mipsel-unknown-linux-musl ramips/mt7621
	package "$(bin mipsel-unknown-linux-musl)" mipsel_24kc
	mips mips-unknown-linux-musl ath79/generic
	package "$(bin mips-unknown-linux-musl)" mips_24kc
}

mkdir -p "$DIST"
[ $# -gt 0 ] || set -- linux-x86_64 linux-aarch64 windows-x86_64 openwrt
for t in "$@"; do
	case $t in
		linux-x86_64) linux x86_64 x86_64-unknown-linux-musl ;;
		linux-aarch64) linux aarch64 aarch64-unknown-linux-musl ;;
		windows-x86_64) windows ;;
		openwrt) openwrt ;;
		*) echo "unknown target $t" >&2; exit 1 ;;
	esac
done
ls -l "$DIST"
