.PHONY: help guard-target-size lock clean build test e2e check fmt fmt-check lint verify image image-push image-remove

SHELL := /bin/bash
.DEFAULT_GOAL := help

CARGO ?= cargo
DOCKER ?= docker
CARGO_BUILD_JOBS ?= 4
VERSION ?= $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
IMAGE ?= ghcr.io/wl4g-blockchain/agentvault:$(VERSION)
PLATFORM ?= linux/amd64
BUILDER_IMAGE ?= docker.io/library/rust:1.91.1-bookworm
RUNTIME_IMAGE ?= gcr.io/distroless/cc-debian12:nonroot
DOCKER_BUILD_ARGS ?=

BUILD_PROXY := $(or $(HTTPS_PROXY),$(https_proxy),$(HTTP_PROXY),$(http_proxy))
ifneq ($(strip $(BUILD_PROXY)),)
DOCKER_BUILD_ARGS += --network=host --build-arg HTTPS_PROXY=$(BUILD_PROXY) --build-arg HTTP_PROXY=$(BUILD_PROXY)
endif

help:
	@printf '%s\n' \
		'AgentVault — process-isolated payment signing' \
		'' \
		'  make build       Build the release walletd binary' \
		'  make test        Run unit and binary tests' \
		'  make e2e         Run the real-process payment-signing E2E' \
		'  make check       Check every Rust target' \
		'  make fmt-check   Verify formatting' \
		'  make lint        Run Clippy with warnings denied' \
		'  make verify      Run the complete PR acceptance gate' \
		'  make image       Build the runtime image' \
		'  make image-push  Push IMAGE after explicit registry login' \
		'' \
		'Overrides: IMAGE=... PLATFORM=linux/amd64 CARGO_BUILD_JOBS=4'

# Protect shared development hosts from unbounded incremental artifacts.
guard-target-size:
	@size_kb=$$(du -sk target 2>/dev/null | cut -f1); \
	  if [ "$${size_kb:-0}" -gt 10485760 ]; then \
	    echo 'target/ exceeds 10 GiB; cleaning before Rust build'; \
	    $(CARGO) clean; \
	  fi

lock:
	$(CARGO) generate-lockfile

clean:
	$(CARGO) clean

build: guard-target-size
	CARGO_BUILD_JOBS=$(CARGO_BUILD_JOBS) $(CARGO) build --release --locked

test: guard-target-size
	CARGO_BUILD_JOBS=$(CARGO_BUILD_JOBS) $(CARGO) test --lib --bins --locked

e2e: guard-target-size
	CARGO_BUILD_JOBS=$(CARGO_BUILD_JOBS) $(CARGO) test --test payment_signing_e2e --locked -- --nocapture

check: guard-target-size
	CARGO_BUILD_JOBS=$(CARGO_BUILD_JOBS) $(CARGO) check --all-targets --locked

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

lint: guard-target-size
	CARGO_BUILD_JOBS=$(CARGO_BUILD_JOBS) $(CARGO) clippy --all-targets --locked -- -D warnings

verify: fmt-check lint test e2e build

image: guard-target-size
	$(DOCKER) buildx build --load \
		--platform '$(PLATFORM)' \
		--tag '$(IMAGE)' \
		--file Dockerfile \
		--build-arg BUILDER_IMAGE='$(BUILDER_IMAGE)' \
		--build-arg RUNTIME_IMAGE='$(RUNTIME_IMAGE)' \
		$(DOCKER_BUILD_ARGS) .

image-push:
	$(DOCKER) push '$(IMAGE)'

image-remove:
	$(DOCKER) image rm '$(IMAGE)'
