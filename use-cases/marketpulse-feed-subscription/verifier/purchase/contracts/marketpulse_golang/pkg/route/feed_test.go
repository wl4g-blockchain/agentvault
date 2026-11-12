package route_test

import (
	"reflect"
	"testing"

	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source/pkg/route"
)

func TestFeedRouterExposesFrameworkComposition(t *testing.T) {
	value := reflect.TypeOf((*route.FeedRouter)(nil))
	if _, ok := value.MethodByName("Register"); !ok {
		t.Fatal("Register missing")
	}
}
