package main

import (
	"log"

	marketpulse "github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg"
)

func main() {
	if err := run(); err != nil {
		log.Fatal(err)
	}
}

func run() error {
	server, err := marketpulse.NewServerFromEnv()
	if err != nil {
		return err
	}
	defer server.Close()
	return server.Run()
}
