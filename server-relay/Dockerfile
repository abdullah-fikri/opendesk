# syntax=docker/dockerfile:1
# Multi-stage build for the OpenDesk relay.

FROM golang:1.26-alpine AS builder
WORKDIR /src

COPY go.mod go.sum ./
RUN go mod download

COPY cmd ./cmd
COPY internal ./internal

ARG VERSION=1.0.0
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath \
    -ldflags="-s -w -X opendesk/server-relay/internal/version.Version=${VERSION}" \
    -o /out/relay ./cmd/relay

FROM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata \
    && addgroup -S opendesk && adduser -S -G opendesk opendesk
WORKDIR /app

COPY --from=builder /out/relay /app/relay
COPY config.json /app/config.json

USER opendesk
EXPOSE 8088
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD wget -qO- http://127.0.0.1:8088/healthz || exit 1

ENTRYPOINT ["/app/relay", "-config", "/app/config.json"]
