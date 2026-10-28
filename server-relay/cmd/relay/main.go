// Command relay runs the OpenDesk relay: the WebSocket rendezvous and fallback data path between desktop and
// Android clients. It wires configuration, storage, the usecase layer and the HTTP transport together.
package main

import (
	"context"
	"errors"
	"flag"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"opendesk/server-relay/internal/config"
	"opendesk/server-relay/internal/handler"
	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/repository/postgres"
	"opendesk/server-relay/internal/udprelay"
	"opendesk/server-relay/internal/usecase"
	"opendesk/server-relay/internal/version"
)

// shutdownGrace is how long in-flight requests get after SIGINT/SIGTERM.
const shutdownGrace = 10 * time.Second

func main() {
	configPath := flag.String("config", "config.json", "path to the JSON configuration file")
	flag.Parse()

	log := slog.New(slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	if err := run(*configPath, log); err != nil {
		log.Error("relay stopped", "err", err)
		os.Exit(1)
	}
}

func run(configPath string, log *slog.Logger) error {
	cfg, err := config.Load(configPath)
	if err != nil {
		return err
	}
	log.Info("starting OpenDesk relay", "version", version.Version, "pid", os.Getpid(), "addr", cfg.Address(),
		"db", cfg.DBHost, "credentials", len(cfg.DeviceCredentials), "p2p_disabled", cfg.P2PDisabled)

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	db, err := postgres.Open(ctx, postgres.Options{
		Host: cfg.DBHost, Port: cfg.DBPort, User: cfg.DBUser, Password: cfg.DBPassword, Name: cfg.DBName,
	}, log)
	if err != nil {
		return err
	}
	defer db.Close()

	var udp *udprelay.Server
	if cfg.UDPRelayPublic != "" && !cfg.P2PDisabled {
		if udp, err = udprelay.Listen(cfg.UDPRelayAddr, cfg.UDPRelayMbps, log); err != nil {
			return err
		}
		go udp.Run(ctx)
		log.Info("UDP relay listening", "addr", cfg.UDPRelayAddr, "public", cfg.UDPRelayPublic, "mbps", cfg.UDPRelayMbps)
	}

	collector := metrics.New()
	relay := usecase.New(
		postgres.NewHostRepository(db),
		postgres.NewSessionRepository(db),
		collector,
		usecase.Options{
			Credentials: cfg.DeviceCredentials, P2PDisabled: cfg.P2PDisabled, Security: postgres.NewSecurityRepository(db),
			UDPRelay: udp != nil,
		},
		log,
	)
	if err := relay.ResetStaleState(ctx); err != nil {
		return err
	}
	if err := relay.LoadSecurity(ctx); err != nil {
		return err
	}

	h := handler.New(relay, collector, handler.Options{
		ReadBufferSize:  cfg.ReadBufferSize,
		WriteBufferSize: cfg.WriteBufferSize,
		PingInterval:    cfg.PingInterval(),
		MetricsToken:    cfg.MetricsToken,
		TLSProxy:        cfg.TLSProxy,
		UDPRelay:        udp,
		UDPRelayPublic:  cfg.UDPRelayPublic,
	}, log)
	go h.RunPinger(ctx)
	go relay.RunJanitor(ctx, cfg.PingInterval())

	server := &http.Server{Addr: cfg.Address(), Handler: h, ReadHeaderTimeout: 10 * time.Second}
	serveErr := make(chan error, 1)
	go func() { serveErr <- server.ListenAndServe() }()
	log.Info("listening", "addr", cfg.Address())

	select {
	case err := <-serveErr:
		if !errors.Is(err, http.ErrServerClosed) {
			return err
		}
		return nil
	case <-ctx.Done():
	}
	log.Info("shutting down")
	shutdownCtx, cancel := context.WithTimeout(context.Background(), shutdownGrace)
	defer cancel()
	// Shutdown does not close hijacked WebSocket connections; the process exit does, after the grace period.
	return server.Shutdown(shutdownCtx)
}
