# Multi-stage image for OpenDesk Relay Gateway
FROM golang:alpine AS builder

WORKDIR /app

# Install git/certs if needed
RUN apk add --no-cache ca-certificates

# Cache dependencies
COPY go.mod go.sum ./
RUN go mod download

# Copy source code
COPY . .

# Build statically linked binary without debug symbols
RUN CGO_ENABLED=0 GOOS=linux go build -ldflags="-s -w" -o server-relay .

# Minimal scratch runtime
FROM alpine:3.20

WORKDIR /app

RUN apk add --no-cache ca-certificates tzdata

COPY --from=builder /app/server-relay /app/server-relay
COPY --from=builder /app/config.json /app/config.json

EXPOSE 8088

ENTRYPOINT ["/app/server-relay"]
