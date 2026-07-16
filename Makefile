.PHONY: build test vet fmt check clean

build:
	mkdir -p bin
	go build -trimpath -ldflags "-s -w" -o bin/grok-build-proxy ./cmd/grok-build-proxy

test:
	go test ./...

vet:
	go vet ./...

fmt:
	gofmt -w $$(find . -name '*.go' -type f)

check: fmt vet test build

clean:
	rm -rf bin dist
