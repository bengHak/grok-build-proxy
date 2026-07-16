BINARY := grok-build-proxy
VERSION ?= dev
HOST_ARCH := $(shell go env GOARCH)
DIST_DIR := dist
LDFLAGS := -s -w -X main.version=$(VERSION)

.PHONY: build build-arm64 build-amd64 dist test vet fmt check clean

build:
	mkdir -p bin
	CGO_ENABLED=0 GOOS=darwin GOARCH=$(HOST_ARCH) \
		go build -trimpath -ldflags "$(LDFLAGS)" \
		-o bin/$(BINARY) ./cmd/grok-build-proxy

build-arm64:
	mkdir -p bin
	CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 \
		go build -trimpath -ldflags "$(LDFLAGS)" \
		-o bin/$(BINARY)-darwin-arm64 ./cmd/grok-build-proxy

build-amd64:
	mkdir -p bin
	CGO_ENABLED=0 GOOS=darwin GOARCH=amd64 \
		go build -trimpath -ldflags "$(LDFLAGS)" \
		-o bin/$(BINARY)-darwin-amd64 ./cmd/grok-build-proxy

dist: clean
	mkdir -p $(DIST_DIR)/arm64 $(DIST_DIR)/amd64
	CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 \
		go build -trimpath -ldflags "$(LDFLAGS)" \
		-o $(DIST_DIR)/arm64/$(BINARY) ./cmd/grok-build-proxy
	CGO_ENABLED=0 GOOS=darwin GOARCH=amd64 \
		go build -trimpath -ldflags "$(LDFLAGS)" \
		-o $(DIST_DIR)/amd64/$(BINARY) ./cmd/grok-build-proxy
	cp LICENSE README.md $(DIST_DIR)/arm64/
	cp LICENSE README.md $(DIST_DIR)/amd64/
	tar -C $(DIST_DIR)/arm64 -czf $(DIST_DIR)/$(BINARY)_Darwin_arm64.tar.gz $(BINARY) LICENSE README.md
	tar -C $(DIST_DIR)/amd64 -czf $(DIST_DIR)/$(BINARY)_Darwin_amd64.tar.gz $(BINARY) LICENSE README.md
	cd $(DIST_DIR) && shasum -a 256 \
		$(BINARY)_Darwin_arm64.tar.gz \
		$(BINARY)_Darwin_amd64.tar.gz > checksums.txt
	rm -rf $(DIST_DIR)/arm64 $(DIST_DIR)/amd64

test:
	go test ./...

vet:
	go vet ./...

fmt:
	gofmt -w $$(find . -name '*.go' -type f)

check: fmt vet test build
	sh -n install.sh

clean:
	rm -rf bin $(DIST_DIR)
