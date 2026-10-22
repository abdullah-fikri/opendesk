package main

import (
	"fmt"
	"net/http"
	"os"
	"time"

	"opendesk/server-relay/internal/config"
	"opendesk/server-relay/internal/database"
	"opendesk/server-relay/internal/gateway"
	"opendesk/server-relay/internal/logger"
)

func main() {
	cfg := config.Load("config.json")

	logger.Info("main", "* OpenDesk Relay Service Startup *")
	logger.Info("main", "* Version 2.5.0 (native-go/anynet-relay + postgresql)")
	logger.Info("main", "Process started. PID %d.", os.Getpid())
	logger.Info("base.config", "Loaded configuration: read_buf=%d write_buf=%d heartbeat_sec=%d db=%s:%d/%s", cfg.ReadBufferSize, cfg.WriteBufferSize, cfg.PingIntervalSeconds, cfg.DBHost, cfg.DBPort, cfg.DBName)

	dbStore, err := database.Connect(cfg)
	if err != nil {
		logger.Error("main", "PostgreSQL connection error: %v", err)
		os.Exit(1)
	}
	defer dbStore.DB.Close()

	gw := gateway.NewGateway(cfg.ReadBufferSize, cfg.WriteBufferSize, dbStore)
	gw.StartHeartbeat(time.Duration(cfg.PingIntervalSeconds) * time.Second)

	addr := fmt.Sprintf("%s:%d", cfg.Host, cfg.Port)
	logger.Info("anynet.tcp_acceptor", "Listening on TCP address %s", addr)

	server := &http.Server{
		Addr:    addr,
		Handler: gw,
	}

	if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		logger.Error("main", "Fatal server error: %v", err)
		os.Exit(1)
	}
}
