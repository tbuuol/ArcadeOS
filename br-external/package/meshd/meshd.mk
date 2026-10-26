################################################################################
#
# meshd – ArcadeOS Mesh-Dienst (Quelle: meshd/ im Repo)
#
################################################################################

MESHD_VERSION = 0.1.0
MESHD_SITE = $(BR2_EXTERNAL_ARCADEOS_PATH)/../meshd
MESHD_SITE_METHOD = local
MESHD_LICENSE = MIT
MESHD_DEPENDENCIES = host-rustc
# Build-Ausgaben vom Entwickeln auf dem Host nicht mitkopieren
MESHD_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude target

MESHD_TARGET_ENV = $(shell echo $(RUSTC_TARGET_NAME) | tr a-z- A-Z_)
MESHD_TARGET_CC_ENV = $(subst -,_,$(RUSTC_TARGET_NAME))

# Crates kommen aus dem Cargo.lock (--locked); Cache ausserhalb des Build-Ordners
define MESHD_BUILD_CMDS
	cd $(@D) && $(TARGET_MAKE_ENV) \
		CARGO_HOME=$(HOME)/.cargo-arcadeos \
		CARGO_TARGET_$(MESHD_TARGET_ENV)_LINKER="$(TARGET_CC)" \
		CC_$(MESHD_TARGET_CC_ENV)="$(TARGET_CC)" \
		AR_$(MESHD_TARGET_CC_ENV)="$(TARGET_AR)" \
		$(HOST_DIR)/bin/cargo build --release --locked --target $(RUSTC_TARGET_NAME)
endef

define MESHD_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/target/$(RUSTC_TARGET_NAME)/release/meshd $(TARGET_DIR)/usr/sbin/meshd
endef

$(eval $(generic-package))
