package storage_test

import (
	"context"
	"errors"
	"github.com/a2aproject/a2a-go/a2a"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/storage"
	"path/filepath"
	"testing"
)

func TestA2ATaskStorePersistsAndRejectsStaleUpdates(t *testing.T) {
	path := filepath.Join(t.TempDir(), "a2a.sqlite")
	repository, err := storage.NewFeedSqliteRepository(path)
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	if _, _, err = repository.Get(ctx, "missing"); !errors.Is(err, a2a.ErrTaskNotFound) {
		t.Fatal(err)
	}
	task := &a2a.Task{ID: "task-1", ContextID: "context-1", Status: a2a.TaskStatus{State: a2a.TaskStateSubmitted}}
	version, err := repository.Save(ctx, task, task, nil, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = repository.Save(ctx, task, task, nil, 0); err == nil {
		t.Fatal("duplicate creation accepted")
	}
	task.Status.State = a2a.TaskStateCompleted
	if _, err = repository.Save(ctx, task, task, nil, version); err != nil {
		t.Fatal(err)
	}
	if _, err = repository.Save(ctx, task, task, nil, version); err == nil {
		t.Fatal("stale version accepted")
	}
	if err = repository.Close(); err != nil {
		t.Fatal(err)
	}
	repository, err = storage.NewFeedSqliteRepository(path)
	if err != nil {
		t.Fatal(err)
	}
	defer repository.Close()
	recovered, recoveredVersion, err := repository.Get(ctx, "task-1")
	if err != nil || recoveredVersion != version+1 || recovered.Status.State != a2a.TaskStateCompleted {
		t.Fatalf("lost task: %+v %d %v", recovered, recoveredVersion, err)
	}
	recovered.ContextID = "mutated"
	fresh, _, _ := repository.Get(ctx, "task-1")
	if fresh.ContextID != "context-1" {
		t.Fatal("Get leaked mutable task state")
	}
}
