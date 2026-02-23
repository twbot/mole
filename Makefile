LEVEL ?= patch

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
MAJOR := $(word 1,$(subst ., ,$(VERSION)))
MINOR := $(word 2,$(subst ., ,$(VERSION)))
PATCH := $(word 3,$(subst ., ,$(VERSION)))

ifeq ($(LEVEL),patch)
  NEXT := $(MAJOR).$(MINOR).$(shell echo $$(($(PATCH)+1)))
else ifeq ($(LEVEL),minor)
  NEXT := $(MAJOR).$(shell echo $$(($(MINOR)+1))).0
else ifeq ($(LEVEL),major)
  NEXT := $(shell echo $$(($(MAJOR)+1))).0.0
endif

release:
	@echo "$(VERSION) → $(NEXT)"
	@sed -i '' 's/^version = "$(VERSION)"/version = "$(NEXT)"/' Cargo.toml
	@cargo check --quiet
	@git add Cargo.toml Cargo.lock
	@git commit -m "v$(NEXT)"
	@git tag "v$(NEXT)"
	@git push origin main
	@git push origin "v$(NEXT)"
	@echo "v$(NEXT) released. Workflow should be triggered to publish to homebrew-tap."

.PHONY: release
