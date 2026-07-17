package monitor

import (
	"strings"
	"unicode"

	"github.com/bengHak/grok-build-proxy/internal/proxy"
)

const textLimit = 256

func sanitizeEvent(event proxy.RequestEvent) proxy.RequestEvent {
	event.RequestID = safeText(event.RequestID)
	event.SessionID = safeText(event.SessionID)
	event.RequestedModel = safeText(event.RequestedModel)
	event.Model = safeText(event.Model)
	event.Error = safeText(event.Error)
	return event
}

func safeText(value string) string {
	runes := make([]rune, 0, textLimit)
	truncated := false
	for _, r := range value {
		if len(runes) == textLimit {
			truncated = true
			break
		}
		if unicode.IsControl(r) || unicode.In(r, unicode.Cf, unicode.Cs) {
			r = ' '
		}
		runes = append(runes, r)
	}
	if truncated {
		runes[len(runes)-1] = '…'
	}
	return strings.TrimSpace(string(runes))
}
