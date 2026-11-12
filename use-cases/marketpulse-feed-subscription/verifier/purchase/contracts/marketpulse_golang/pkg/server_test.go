package marketpulse_test

import (
	"reflect"
	"testing"

	marketpulse "github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source/pkg"
)

func TestServerOwnsListenerAndLifecycle(t *testing.T) {
	typeOf := reflect.TypeOf((*marketpulse.Server)(nil))
	for _, method := range []string{"Run", "Close"} {
		if _, ok := typeOf.MethodByName(method); !ok {
			t.Fatalf("%s missing", method)
		}
	}
}
