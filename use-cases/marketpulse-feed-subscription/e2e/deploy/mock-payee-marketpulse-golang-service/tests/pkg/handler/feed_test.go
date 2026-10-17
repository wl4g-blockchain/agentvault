package handler_test

import (
	"reflect"
	"testing"

	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/handler"
)

func TestFeedHandlerOwnsOperationalAndMCPBusinessOperations(t *testing.T) {
	value := reflect.TypeOf((*handler.FeedHandler)(nil))
	for _, method := range []string{"Health", "Metrics", "MCP"} {
		if _, ok := value.MethodByName(method); !ok {
			t.Fatalf("%s missing", method)
		}
	}
}
