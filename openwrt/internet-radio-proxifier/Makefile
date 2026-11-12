# SPDX-License-Identifier: LGPL-2.1-only
#
# OpenWrt SDK package for internet-radio-proxifier. Builds from this repository's sources; crates are downloaded at
# build time (versions pinned by Cargo.lock). Link this directory into the
# SDK's package/ folder, so it stays inside the repository:
#   ln -s /path/to/internet-radio-proxifier/openwrt/internet-radio-proxifier package/internet-radio-proxifier
#   ./scripts/feeds update packages && ./scripts/feeds install rust
#   make package/internet-radio-proxifier/compile V=s
# Works with the OpenWrt 25.x (apk) and 24.10 (ipk) SDKs.

include $(TOPDIR)/rules.mk

PKG_DIR:=$(realpath $(CURDIR))
REPO_DIR:=$(realpath $(PKG_DIR)/../..)

PKG_NAME:=internet-radio-proxifier
PKG_VERSION:=$(shell tr -d ' \r\n' < $(REPO_DIR)/VERSION)
PKG_RELEASE:=1

PKG_LICENSE:=LGPL-2.1
PKG_LICENSE_FILES:=LICENSE
PKG_MAINTAINER:=stavultras

PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/internet-radio-proxifier
  SECTION:=multimedia
  CATEGORY:=Multimedia
  TITLE:=Web radio player with a per-station proxy
  URL:=https://github.com/stavultras/internet-radio-proxifier
  DEPENDS:=$(RUST_ARCH_DEPENDS) +ca-bundle
endef

define Package/internet-radio-proxifier/description
  Web radio player that streams internet radio stations through the
  router, with a settings page and an optional SOCKS5/HTTP proxy per
  station. Listens on port 9876; service options in /etc/config/internet-radio-proxifier,
  proxy and station list in /etc/internet-radio-proxifier/settings.ini.
endef

define Package/internet-radio-proxifier/conffiles
/etc/config/internet-radio-proxifier
/etc/internet-radio-proxifier/
endef

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) $(REPO_DIR)/Cargo.toml $(REPO_DIR)/Cargo.lock $(REPO_DIR)/VERSION \
		$(REPO_DIR)/LICENSE $(REPO_DIR)/settings.example.ini \
		$(REPO_DIR)/src $(REPO_DIR)/assets $(PKG_BUILD_DIR)/
endef

define Package/internet-radio-proxifier/install
	$(INSTALL_DIR) $(1)/usr/bin $(1)/etc/init.d $(1)/etc/config $(1)/usr/share/internet-radio-proxifier $(1)/lib/upgrade/keep.d
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/internet-radio-proxifier $(1)/usr/bin/internet-radio-proxifier
	$(INSTALL_BIN) $(REPO_DIR)/openwrt/files/internet-radio-proxifier.init $(1)/etc/init.d/internet-radio-proxifier
	$(INSTALL_CONF) $(REPO_DIR)/openwrt/files/radio.config $(1)/etc/config/internet-radio-proxifier
	$(INSTALL_DATA) $(REPO_DIR)/settings.example.ini $(1)/usr/share/internet-radio-proxifier/settings.example.ini
	echo /etc/internet-radio-proxifier/ > $(1)/lib/upgrade/keep.d/internet-radio-proxifier
endef

# default_postinst only "start"s the service, which procd ignores when it is
# already running, so restart a running (old) one to run the new binary.
# OpenWrt 24.10's opkg does not set PKG_UPGRADE, so check for the process.
define Package/internet-radio-proxifier/postinst
#!/bin/sh
[ -z "$${IPKG_INSTROOT}" ] && pidof internet-radio-proxifier >/dev/null && /etc/init.d/internet-radio-proxifier restart
exit 0
endef

$(eval $(call BuildPackage,internet-radio-proxifier))
