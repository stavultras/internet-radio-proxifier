# SPDX-License-Identifier: LGPL-2.1-only
#
# OpenWrt SDK package for internet-radio-proxifier (binary and service
# "radio"). Builds from this repository's sources; crates are downloaded at
# build time (versions pinned by Cargo.lock). Link this directory into the
# SDK's package/ folder, so it stays inside the repository:
#   ln -s /path/to/internet-radio-proxifier/openwrt/radio package/radio
#   ./scripts/feeds update packages && ./scripts/feeds install rust
#   make package/radio/compile V=s

include $(TOPDIR)/rules.mk

PKG_DIR:=$(realpath $(CURDIR))
REPO_DIR:=$(realpath $(PKG_DIR)/../..)

PKG_NAME:=radio
PKG_VERSION:=$(shell tr -d ' \r\n' < $(REPO_DIR)/VERSION)
PKG_RELEASE:=1

PKG_LICENSE:=LGPL-2.1
PKG_LICENSE_FILES:=LICENSE
PKG_MAINTAINER:=stavultras

PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/radio
  SECTION:=multimedia
  CATEGORY:=Multimedia
  TITLE:=Web radio player with a per-station proxy
  URL:=https://github.com/stavultras/internet-radio-proxifier
  DEPENDS:=$(RUST_ARCH_DEPENDS) +ca-bundle
endef

define Package/radio/description
  Web radio player that streams internet radio stations through the
  router, with a settings page and an optional SOCKS5/HTTP proxy per
  station. Listens on port 8123; service options in /etc/config/radio,
  proxy and station list in /etc/radio/settings.ini.
endef

define Package/radio/conffiles
/etc/config/radio
/etc/radio/
endef

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) $(REPO_DIR)/Cargo.toml $(REPO_DIR)/Cargo.lock $(REPO_DIR)/VERSION \
		$(REPO_DIR)/LICENSE $(REPO_DIR)/settings.example.ini \
		$(REPO_DIR)/src $(REPO_DIR)/assets $(PKG_BUILD_DIR)/
endef

define Package/radio/install
	$(INSTALL_DIR) $(1)/usr/bin $(1)/etc/init.d $(1)/etc/config $(1)/usr/share/radio $(1)/lib/upgrade/keep.d
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/radio $(1)/usr/bin/radio
	$(INSTALL_BIN) $(REPO_DIR)/openwrt/files/radio.init $(1)/etc/init.d/radio
	$(INSTALL_CONF) $(REPO_DIR)/openwrt/files/radio.config $(1)/etc/config/radio
	$(INSTALL_DATA) $(REPO_DIR)/settings.example.ini $(1)/usr/share/radio/settings.example.ini
	echo /etc/radio/ > $(1)/lib/upgrade/keep.d/radio
endef

# default_postinst only "start"s the service, which procd ignores when it is
# already running, so restart it on upgrade to run the new binary.
define Package/radio/postinst
#!/bin/sh
[ -n "$${PKG_UPGRADE}" ] && [ -z "$${IPKG_INSTROOT}" ] && /etc/init.d/radio restart
exit 0
endef

$(eval $(call BuildPackage,radio))
