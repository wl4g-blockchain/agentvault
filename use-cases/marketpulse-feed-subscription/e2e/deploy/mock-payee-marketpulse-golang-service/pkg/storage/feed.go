package storage

import (
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	_ "github.com/mattn/go-sqlite3"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

var actors = [][2]string{
	{"Donald Trump", "United States president"}, {"Marco Rubio", "United States secretary of state"},
	{"Ursula von der Leyen", "European Commission president"}, {"Keir Starmer", "United Kingdom prime minister"},
	{"Emmanuel Macron", "French president"}, {"Friedrich Merz", "German chancellor"},
	{"Shigeru Ishiba", "Japanese prime minister"}, {"Narendra Modi", "Indian prime minister"},
	{"Luiz Inacio Lula da Silva", "Brazilian president"}, {"Anthony Albanese", "Australian prime minister"},
}

var topics = [][3]string{
	{"tariff", "proposed reciprocal tariff review for strategic imports", "SPY"},
	{"chips", "outlined semiconductor export-control consultations", "SOXX"},
	{"energy", "commented on energy-security supply coordination", "XLE"},
	{"shipping", "addressed Red Sea shipping-risk countermeasures", "BDRY"},
	{"defense", "discussed allied defense procurement commitments", "ITA"},
	{"currency", "responded to cross-border currency volatility", "DXY"},
	{"sanctions", "announced a review of targeted trade sanctions", "EEM"},
	{"agriculture", "set out agricultural import quota negotiations", "DBA"},
	{"critical-minerals", "signed a critical-minerals supply memorandum", "LIT"},
	{"digital-trade", "opened digital-services tax and AI trade talks", "QQQ"},
}

type NewsItem struct {
	ID           string   `json:"id"`
	ObservedAt   string   `json:"observedAt"`
	Author       string   `json:"author"`
	Role         string   `json:"role"`
	Topic        string   `json:"topic"`
	Symbols      []string `json:"symbols"`
	Headline     string   `json:"headline"`
	Source       string   `json:"source"`
	SentimentBPS int      `json:"sentimentBps"`
}

type DeliveryState struct {
	IntentID string
	Response string
}

type FeedSqliteRepository struct {
	database *sql.DB
}

func NewFeedSqliteRepository(path string) (*FeedSqliteRepository, error) {
	if path == "" || path == ":memory:" {
		return nil, errors.New("MARKETPULSE_DATABASE must be persistent")
	}
	database, err := sql.Open("sqlite3", path+"?_foreign_keys=on&_journal_mode=DELETE&_synchronous=FULL")
	if err != nil {
		return nil, fmt.Errorf("open MarketPulse database: %w", err)
	}
	storage := &FeedSqliteRepository{database: database}
	if err := storage.initialize(); err != nil {
		database.Close()
		return nil, err
	}
	return storage, nil
}

func (storage *FeedSqliteRepository) Close() error { return storage.database.Close() }

func (storage *FeedSqliteRepository) initialize() error {
	_, err := storage.database.Exec(`
CREATE TABLE IF NOT EXISTS a2a_task(id TEXT PRIMARY KEY,version INTEGER NOT NULL,payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS feed_item(
 id TEXT PRIMARY KEY,observed_at TEXT NOT NULL,author TEXT NOT NULL,role TEXT NOT NULL,topic TEXT NOT NULL,
 symbols_json TEXT NOT NULL,headline TEXT NOT NULL,source TEXT NOT NULL,sentiment_bps INTEGER NOT NULL,
 access_tier TEXT NOT NULL CHECK(access_tier IN ('yesterday','realtime')));
CREATE TABLE IF NOT EXISTS mcp_delivery(
 run_id TEXT PRIMARY KEY,request_hash TEXT NOT NULL,intent_id TEXT NOT NULL,settlement_json TEXT NOT NULL,
 response_json TEXT,status TEXT NOT NULL CHECK(status IN ('settled','delivered')),
 failure_injections INTEGER NOT NULL DEFAULT 0 CHECK(failure_injections IN (0,1)),updated_at INTEGER NOT NULL);`)
	if err != nil {
		return fmt.Errorf("initialize MarketPulse database: %w", err)
	}
	var count int
	if err := storage.database.QueryRow("SELECT COUNT(*) FROM feed_item").Scan(&count); err != nil {
		return err
	}
	if count == 100 {
		return nil
	}
	if count != 0 {
		return fmt.Errorf("MarketPulse feed seed must contain exactly 100 rows, found %d", count)
	}
	transaction, err := storage.database.Begin()
	if err != nil {
		return err
	}
	defer transaction.Rollback()
	for index := 0; index < 100; index++ {
		actor := actors[index%len(actors)]
		topic := topics[(index/len(actors))%len(topics)]
		tier, source, day := "yesterday", "MarketPulse public archive", 14
		if index >= 50 {
			tier, source, day = "realtime", "MarketPulse live wire", 15
		}
		symbols, _ := json.Marshal([]string{topic[2]})
		_, err = transaction.Exec("INSERT INTO feed_item VALUES(?,?,?,?,?,?,?,?,?,?)",
			fmt.Sprintf("marketpulse-%03d", index), fmt.Sprintf("2026-10-%02dT%02d:%02d:00Z", day, index%24, index%60),
			actor[0], actor[1], topic[0], string(symbols),
			fmt.Sprintf("%s %s; markets assess geopolitical and tariff implications", actor[0], topic[1]),
			source, (index-50)*7, tier)
		if err != nil {
			return fmt.Errorf("insert feed seed: %w", err)
		}
	}
	return transaction.Commit()
}

func (storage *FeedSqliteRepository) News(tier string, limit int) ([]NewsItem, error) {
	rows, err := storage.database.Query(`SELECT id,observed_at,author,role,topic,symbols_json,headline,source,sentiment_bps
FROM feed_item WHERE access_tier=? ORDER BY id LIMIT ?`, tier, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]NewsItem, 0, limit)
	for rows.Next() {
		var item NewsItem
		var symbols string
		if err := rows.Scan(&item.ID, &item.ObservedAt, &item.Author, &item.Role, &item.Topic, &symbols,
			&item.Headline, &item.Source, &item.SentimentBPS); err != nil {
			return nil, err
		}
		if err := json.Unmarshal([]byte(symbols), &item.Symbols); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (storage *FeedSqliteRepository) Delivery(runID, requestHash string) (DeliveryState, error) {
	var storedHash, intentID, response, status string
	err := storage.database.QueryRow(`SELECT request_hash,intent_id,COALESCE(response_json,''),status
FROM mcp_delivery WHERE run_id=?`, runID).Scan(&storedHash, &intentID, &response, &status)
	if errors.Is(err, sql.ErrNoRows) {
		return DeliveryState{}, nil
	}
	if err != nil {
		return DeliveryState{}, err
	}
	if storedHash != requestHash || (status != "settled" && status != "delivered") {
		return DeliveryState{}, errors.New("MCP run_id conflicts with durable delivery")
	}
	return DeliveryState{IntentID: intentID, Response: response}, nil
}

func (storage *FeedSqliteRepository) RecordSettled(
	runID, requestHash string,
	payment *model.SettledIncomingPayment,
) error {
	if payment.Intent.RunID != runID {
		return errors.New("settlement is not bound to the workflow run")
	}
	encoded, err := json.Marshal(payment)
	if err != nil {
		return err
	}
	result, err := storage.database.Exec(`INSERT INTO mcp_delivery
(run_id,request_hash,intent_id,settlement_json,response_json,status,updated_at)
VALUES(?,?,?,?,NULL,'settled',?) ON CONFLICT(run_id) DO NOTHING`,
		runID, requestHash, payment.Intent.ID, string(encoded), time.Now().Unix())
	if err != nil {
		return err
	}
	inserted, _ := result.RowsAffected()
	if inserted == 0 {
		state, readErr := storage.Delivery(runID, requestHash)
		if readErr != nil || state.IntentID != payment.Intent.ID {
			return errors.New("MCP run_id is already bound to another settlement")
		}
	}
	return nil
}

func (storage *FeedSqliteRepository) RecordDelivered(runID, requestHash, response string) error {
	result, err := storage.database.Exec(`UPDATE mcp_delivery SET response_json=?,status='delivered',updated_at=?
WHERE run_id=? AND request_hash=? AND status='settled'`, response, time.Now().Unix(), runID, requestHash)
	if err != nil {
		return err
	}
	changed, _ := result.RowsAffected()
	if changed != 1 {
		return errors.New("MCP delivery was not in settled state")
	}
	return nil
}

func (storage *FeedSqliteRepository) RecordFailure(runID, requestHash string) error {
	result, err := storage.database.Exec(`UPDATE mcp_delivery SET failure_injections=1,updated_at=?
WHERE run_id=? AND request_hash=? AND status='settled' AND failure_injections=0`,
		time.Now().Unix(), runID, requestHash)
	if err != nil {
		return err
	}
	changed, _ := result.RowsAffected()
	if changed != 1 {
		return errors.New("post-settlement failure injection was already consumed")
	}
	return nil
}
