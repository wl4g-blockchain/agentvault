.PHONY: help image-source guard-target-size lock clean build test e2e check fmt fmt-check lint verify image image-push image-remove

SHELL := /bin/bash
.DEFAULT_GOAL := help

CARGO ?= cargo
DOCKER ?= docker
CARGO_BUILD_JOBS ?= 4
E2E_LEVEL ?= full
E2E_PAYER_LANGUAGE ?= rust
E2E_PAYEE_LANGUAGE ?= rust
E2E_TIMEOUT ?= 45m
VERSION ?= $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
IMAGE ?= ghcr.io/wl4g-blockchain/agentvault:$(VERSION)
PLATFORM ?= linux/amd64
DOCKER_BUILD_ARGS ?=

# Registry reachability is probed only for targets that consume container
# images. Local formatting, compilation, and unit tests never make this
# unrelated network request.
IMAGE_SOURCE_GOALS := image-source image image-push e2e
ifneq ($(strip $(filter $(IMAGE_SOURCE_GOALS),$(MAKECMDGOALS))),)
ifeq ($(origin IN_CN_GFW),undefined)
IN_CN_GFW := $(shell \
	if command -v curl >/dev/null 2>&1; then \
		status=$$(curl -sS --noproxy '*' -o /dev/null -w '%{http_code}' \
			--connect-timeout 3 --max-time 5 https://registry-1.docker.io/v2/ \
			2>/dev/null || true); \
		if [ "$$status" = 200 ] || [ "$$status" = 401 ]; then echo false; else echo true; fi; \
	else echo true; fi)
endif
else ifeq ($(origin IN_CN_GFW),undefined)
IN_CN_GFW := false
endif
ifeq ($(filter $(IN_CN_GFW),true false),)
$(error IN_CN_GFW must be true or false)
endif

IMAGE_SOURCE ?= $(if $(filter true,$(IN_CN_GFW)),aliyun,global)
ifeq ($(filter $(IMAGE_SOURCE),aliyun global),)
$(error IMAGE_SOURCE must be aliyun or global)
endif
AGENTVAULT_E2E_IMAGE_SOURCE ?= $(IMAGE_SOURCE)
ifeq ($(filter $(AGENTVAULT_E2E_IMAGE_SOURCE),aliyun global),)
$(error AGENTVAULT_E2E_IMAGE_SOURCE must be aliyun or global)
endif
export AGENTVAULT_E2E_IMAGE_SOURCE

ifeq ($(IMAGE_SOURCE),aliyun)
BUILDER_IMAGE ?= registry.cn-shenzhen.aliyuncs.com/wl4g/authguard-rust-builder:1.94-bookworm
RUNTIME_IMAGE ?= registry.cn-shenzhen.aliyuncs.com/wl4g/authguard-runtime:nonroot
else
BUILDER_IMAGE ?= docker.io/library/rust:1.91.1-bookworm
RUNTIME_IMAGE ?= gcr.io/distroless/cc-debian12:nonroot
endif

BUILD_PROXY := $(or $(HTTPS_PROXY),$(https_proxy),$(HTTP_PROXY),$(http_proxy))
ifneq ($(strip $(BUILD_PROXY)),)
DOCKER_BUILD_ARGS += --network=host --build-arg HTTPS_PROXY=$(BUILD_PROXY) --build-arg HTTP_PROXY=$(BUILD_PROXY)
endif

help:
	@printf '%s\n' \
		'AgentVault — enterprise AI-agent payment infrastructure' \
		'' \
		'  make build       Build the release agentvault binary' \
		'  make test        Run unit and binary tests' \
		'  make e2e         Run the real Docker payment E2E (default: full rust)' \
		'  make check       Check every Rust target' \
		'  make fmt-check   Verify formatting' \
		'  make lint        Run Clippy with warnings denied' \
		'  make verify      Run the complete PR acceptance gate' \
		'  make image-source Show detected registry/image selection' \
		'  make image       Build the runtime image' \
		'  make image-push  Push IMAGE after explicit registry login' \
		'' \
		'Overrides: IMAGE=... PLATFORM=linux/amd64 CARGO_BUILD_JOBS=4 E2E_LEVEL=full E2E_PAYER_LANGUAGE=go E2E_PAYEE_LANGUAGE=java E2E_TIMEOUT=45m'

image-source:
	@echo "IN_CN_GFW=$(IN_CN_GFW)"
	@echo "IMAGE_SOURCE=$(IMAGE_SOURCE)"
	@echo "AGENTVAULT_E2E_IMAGE_SOURCE=$(AGENTVAULT_E2E_IMAGE_SOURCE)"
	@echo "BUILDER_IMAGE=$(BUILDER_IMAGE)"
	@echo "RUNTIME_IMAGE=$(RUNTIME_IMAGE)"

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
	timeout --signal=TERM --kill-after=2m $(E2E_TIMEOUT) \
		python3 use-cases/marketpulse-feed-subscription/e2e/runner.py \
		--scenario $(E2E_LEVEL) \
		--payer-language $(E2E_PAYER_LANGUAGE) \
		--payee-language $(E2E_PAYEE_LANGUAGE) \
		--timeout $(E2E_TIMEOUT)

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
