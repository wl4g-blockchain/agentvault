package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang/pkg/handler"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang/pkg/job"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang/pkg/route"
)

func main() {
	once := flag.Bool("once", false, "run one ingestion cycle")
	flag.Parse()
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if *once {
		ingestion, err := job.NewFromEnv(ctx)
		if err == nil {
			defer ingestion.Close()
			err = ingestion.RunOnce(ctx, fmt.Sprintf("go-%d", time.Now().Unix()))
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	server := &http.Server{
		Addr:              ":8080",
		Handler:           route.NewIngestionRouter(handler.NewIngestionHandler()).Engine(),
		ReadHeaderTimeout: 5 * time.Second,
	}
	go func() {
		ingestion, err := job.NewFromEnv(ctx)
		if err == nil {
			defer ingestion.Close()
			err = ingestion.RunScheduler(ctx)
		}
		if err != nil && !errors.Is(err, context.Canceled) {
			fmt.Fprintf(os.Stderr, "background ingestion stopped: %v\n", err)
			stop()
		}
	}()
	go func() {
		<-ctx.Done()
		shutdown, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = server.Shutdown(shutdown)
	}()
	if err := server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
