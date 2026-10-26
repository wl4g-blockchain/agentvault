package storage_test

import (
	"path/filepath"
	"testing"

	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/storage"
)

func TestFeedSqliteRepositorySeedsOneHundredRows(t *testing.T) {
	database, err := storage.NewFeedSqliteRepository(filepath.Join(t.TempDir(), "marketpulse.sqlite3"))
	if err != nil {
		t.Fatal(err)
	}
	defer database.Close()
	yesterday, _ := database.News("yesterday", 100)
	realtime, _ := database.News("realtime", 100)
	if len(yesterday) != 50 || len(realtime) != 50 {
		t.Fatalf("expected 50/50 seed split, got %d/%d", len(yesterday), len(realtime))
	}
}
