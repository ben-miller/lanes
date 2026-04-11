SHA := $(shell git rev-parse HEAD)
DIR := $(shell pwd)
BIN := $(HOME)/go/bin/spinner

LDFLAGS := -X github.com/bmiller/spinner/internal/build.Version=$(SHA) \
           -X github.com/bmiller/spinner/internal/build.SourceDir=$(DIR) \
           -X github.com/bmiller/spinner/internal/build.Mode=development

.PHONY: build install test

build:
	go build -ldflags "$(LDFLAGS)" -o spinner .

install:
	go build -ldflags "$(LDFLAGS)" -o $(BIN) .

test:
	go test ./...
