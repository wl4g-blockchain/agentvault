package job

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	_ "github.com/mattn/go-sqlite3"
	"github.com/robfig/cron/v3"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang/pkg/agent"
)

type IngestionJob struct {
	mcp                 *agent.MCPHelper
	database            string
	yesterdayCron       string
	realtimeCron        string
	failAfterSettlement bool
}

func NewFromEnv(ctx context.Context) (*IngestionJob, error) {
	service, err := agent.NewMCPHelperFromEnv(ctx)
	if err != nil {
		return nil, err
	}
	database := os.Getenv("QUANTSTREAM_DATABASE")
	if strings.TrimSpace(database) == "" || database == ":memory:" {
		return nil, errors.New("QUANTSTREAM_DATABASE must be persistent")
	}
	return &IngestionJob{mcp: service, database: database,
		yesterdayCron:       valueOr(os.Getenv("QUANTSTREAM_YESTERDAY_CRON"), "0 0 2 * * *"),
		realtimeCron:        valueOr(os.Getenv("QUANTSTREAM_REALTIME_CRON"), "0 */1 * * * *"),
		failAfterSettlement: os.Getenv("QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT") == "once"}, nil
}

func (job *IngestionJob) Close() error { return job.mcp.Close() }

// IngestYesterdayNews is the free delayed-news cron function.
func (job *IngestionJob) IngestYesterdayNews(ctx context.Context, runID string) (agent.ToolResult, error) {
	return job.mcp.YesterdayNews(ctx, runID)
}

// IngestRealtimeNews is the paid realtime-news cron function.
func (job *IngestionJob) IngestRealtimeNews(ctx context.Context, runID string, injectFailure bool) (agent.ToolResult, error) {
	return job.mcp.RealtimeNews(ctx, runID, injectFailure)
}

func (job *IngestionJob) RunOnce(ctx context.Context, runID string) error {
	free, err := job.IngestYesterdayNews(ctx, runID)
	if err != nil {
		return err
	}
	paid, err := job.IngestRealtimeNews(ctx, runID, job.failAfterSettlement)
	if err != nil {
		return err
	}
	if err := persistRun(job.database, runID, free, paid); err != nil {
		return err
	}
	fmt.Printf("QuantStream Go completed run %s\n", runID)
	return nil
}

func (job *IngestionJob) RunScheduler(ctx context.Context) error {
	scheduler := cron.New(cron.WithSeconds())
	if _, err := scheduler.AddFunc(job.yesterdayCron, func() {
		runID := fmt.Sprintf("go-yesterday-%d", time.Now().Unix())
		if _, ingestErr := job.IngestYesterdayNews(ctx, runID); ingestErr != nil {
			fmt.Fprintf(os.Stderr, "delayed-news ingestion failed: %v\n", ingestErr)
		}
	}); err != nil {
		return err
	}
	if _, err := scheduler.AddFunc(job.realtimeCron, func() {
		runID := fmt.Sprintf("go-realtime-%d", time.Now().Unix())
		if _, ingestErr := job.IngestRealtimeNews(ctx, runID, false); ingestErr != nil {
			fmt.Fprintf(os.Stderr, "realtime-news ingestion failed: %v\n", ingestErr)
		}
	}); err != nil {
		return err
	}
	scheduler.Start()
	<-ctx.Done()
	stopped := scheduler.Stop()
	<-stopped.Done()
	return nil
}

func persistRun(path, runID string, free, paid agent.ToolResult) error {
	if !json.Valid(free.Body) || !json.Valid(paid.Body) || paid.PaymentIntentID == "" {
		return errors.New("invalid ingestion evidence")
	}
	database, err := sql.Open("sqlite3", path+"?_foreign_keys=on&_journal_mode=DELETE&_synchronous=FULL")
	if err != nil {
		return err
	}
	defer database.Close()
	if _, err = database.Exec(`CREATE TABLE IF NOT EXISTS ingestion_run (
      run_id TEXT PRIMARY KEY, free_response_json TEXT NOT NULL, paid_response_json TEXT NOT NULL,
      payment_intent_id TEXT NOT NULL UNIQUE, completed_at INTEGER NOT NULL)`); err != nil {
		return err
	}
	transaction, err := database.Begin()
	if err != nil {
		return err
	}
	defer transaction.Rollback()
	if _, err = transaction.Exec(`INSERT INTO ingestion_run(run_id,free_response_json,paid_response_json,payment_intent_id,completed_at)
      VALUES(?,?,?,?,?)`, runID, string(free.Body), string(paid.Body), paid.PaymentIntentID, time.Now().Unix()); err != nil {
		return err
	}
	return transaction.Commit()
}

func valueOr(value, fallback string) string {
	if strings.TrimSpace(value) == "" {
		return fallback
	}
	return value
}
