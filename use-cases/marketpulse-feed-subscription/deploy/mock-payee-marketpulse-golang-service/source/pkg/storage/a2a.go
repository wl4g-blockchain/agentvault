package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/a2aproject/a2a-go/a2a"
	"github.com/a2aproject/a2a-go/a2asrv"
)

var _ a2asrv.TaskStore = (*FeedSqliteRepository)(nil)

// Save persists official A2A Tasks with CAS alongside durable business delivery.
func (repository *FeedSqliteRepository) Save(ctx context.Context, task *a2a.Task, _ a2a.Event, _ *a2a.Task, previous a2a.TaskVersion) (a2a.TaskVersion, error) {
	if task == nil || task.ID == "" {
		return 0, errors.New("A2A task id is required")
	}
	raw, err := json.Marshal(task)
	if err != nil {
		return 0, err
	}
	next := previous + 1
	var result sql.Result
	if previous == 0 {
		result, err = repository.database.ExecContext(ctx, "INSERT INTO a2a_task(id,version,payload) VALUES(?,?,?) ON CONFLICT(id) DO NOTHING", string(task.ID), next, string(raw))
	} else {
		result, err = repository.database.ExecContext(ctx, "UPDATE a2a_task SET version=?,payload=? WHERE id=? AND version=?", next, string(raw), string(task.ID), previous)
	}
	if err != nil {
		return 0, err
	}
	count, err := result.RowsAffected()
	if err != nil {
		return 0, err
	}
	if count != 1 {
		return 0, fmt.Errorf("A2A task %s version conflict", task.ID)
	}
	return next, nil
}
func (repository *FeedSqliteRepository) Get(ctx context.Context, id a2a.TaskID) (*a2a.Task, a2a.TaskVersion, error) {
	var raw string
	var version a2a.TaskVersion
	if err := repository.database.QueryRowContext(ctx, "SELECT payload,version FROM a2a_task WHERE id=?", string(id)).Scan(&raw, &version); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, 0, a2a.ErrTaskNotFound
		}
		return nil, 0, err
	}
	var task a2a.Task
	if err := json.Unmarshal([]byte(raw), &task); err != nil {
		return nil, 0, err
	}
	return &task, version, nil
}
func (repository *FeedSqliteRepository) List(_ context.Context, _ *a2a.ListTasksRequest) (*a2a.ListTasksResponse, error) {
	return nil, errors.New("this E2E agent exposes only non-streaming message/send")
}
