package handler

import (
	"net/http"

	"github.com/gin-gonic/gin"
)

// IngestionHandler maps HTTP requests to the scheduling/application boundary.
type IngestionHandler struct{}

func NewIngestionHandler() *IngestionHandler { return &IngestionHandler{} }

func (handler *IngestionHandler) Health(context *gin.Context) {
	context.JSON(http.StatusOK, gin.H{"status": "ready", "service": "quantstream-golang"})
}

func (handler *IngestionHandler) Metrics(context *gin.Context) {
	context.Data(http.StatusOK, "text/plain; version=0.0.4", []byte(
		"# HELP quantstream_up QuantStream process readiness.\n# TYPE quantstream_up gauge\nquantstream_up 1\n",
	))
}
