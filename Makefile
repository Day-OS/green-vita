.PHONY: vpk eboot upload-vpk update-run-vita run-vita

RUSTFLAGS ?= -C target-feature=-neon
CARGO_VITA ?= cargo +nightly vita
VPK := target/armv7-sony-vita-newlibeabihf/release/green-vita.vpk
VITA_UPLOAD_DIR ?= ux0:/data/

vpk:
	RUSTFLAGS="$(RUSTFLAGS)" $(CARGO_VITA) build vpk --release

eboot:
	RUSTFLAGS="$(RUSTFLAGS)" $(CARGO_VITA) build eboot --release

upload-vpk: vpk
ifndef VITA_IP
	$(error Usage: make upload-vpk VITA_IP=192.168.0.103)
endif
	$(CARGO_VITA) upload --vita-ip $(VITA_IP) --source $(VPK) --destination $(VITA_UPLOAD_DIR)

update-run-vita:
ifndef VITA_IP
	$(error Usage: make update-run-vita VITA_IP=192.168.0.103)
endif
	RUSTFLAGS="$(RUSTFLAGS)" $(CARGO_VITA) build eboot --update --run --vita-ip $(VITA_IP) -- --release

run-vita: update-run-vita
