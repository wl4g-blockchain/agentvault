.PHONY: help build test e2e fmt fmt-check lint check verify lock clean image image-remove

CARGO ?= cargo
DOCKER ?= docker
IMAGE ?= agent-wallet:dev
BUILDER_IMAGE ?= registry.cn-shenzhen.aliyuncs.com/wl4g/golang:1.26-alpine
RUNTIME_IMAGE ?= registry.cn-shenzhen.aliyuncs.com/wl4g/alpine:3.21
DOCKER_BUILD_ARGS ?=

help:
	@echo "Wallet service"
	@echo "  make build  Build a release walletd binary"
	@echo "  make test   Run unit and binary tests"
	@echo "  make e2e    Run the walletd local-transport E2E test"
	@echo "  make check  Check all targets"
	@echo "  make fmt    Format Rust sources"
	@echo "  make fmt-check  Verify formatting without modifying sources"
	@echo "  make lint   Run Clippy with warnings denied"
	@echo "  make verify Run formatting, lint, unit, E2E, and release-build checks"
	@echo "  make lock   Refresh Cargo.lock"
	@echo "  make clean  Remove Wallet Rust build artifacts"
	@echo "  make image  Build the standalone container image ($(IMAGE))"
	@echo "  make image-remove  Remove the selected Wallet image ($(IMAGE))"

lock:
	$(CARGO) generate-lockfile

clean:
	$(CARGO) clean

build:
	$(CARGO) build --release --locked

test:
	$(CARGO) test --lib --bins --locked

e2e:
	$(CARGO) test --test e2e_local --locked

check:
	$(CARGO) check --all-targets --locked

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

lint:
	$(CARGO) clippy --all-targets --locked -- -D warnings

verify:
	$(MAKE) fmt-check
	$(MAKE) lint
	$(MAKE) test
	$(MAKE) e2e
	$(MAKE) build

image:
	$(DOCKER) build $(DOCKER_BUILD_ARGS) \
		--build-arg BUILDER_IMAGE=$(BUILDER_IMAGE) \
		--build-arg RUNTIME_IMAGE=$(RUNTIME_IMAGE) \
		--tag $(IMAGE) .

image-remove:
	$(DOCKER) image rm $(IMAGE)
