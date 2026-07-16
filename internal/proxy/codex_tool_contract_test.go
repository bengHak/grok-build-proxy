package proxy

import (
	"net/http"
	"strings"
	"testing"
)

func TestNormalizeCodexBodyPreservesExplicitToolChoice(t *testing.T) {
	for _, choice := range []any{
		"none",
		"required",
		map[string]any{"type": "function", "name": "exit_plan_mode"},
	} {
		body := map[string]any{"model": "gpt-5.6-sol", "input": "hello", "tool_choice": choice}
		if err := normalizeCodexBody(body, codexIdentity{}, true); err != nil {
			t.Fatalf("choice %#v: %v", choice, err)
		}
		if got := body["tool_choice"]; got == "auto" {
			t.Fatalf("choice %#v was overwritten with auto", choice)
		}
	}
}

func TestNormalizeCodexBodyRejectsInvalidToolChoice(t *testing.T) {
	body := map[string]any{"model": "gpt-5.6-sol", "input": "hello", "tool_choice": 3}
	if err := normalizeCodexBody(body, codexIdentity{}, true); err == nil {
		t.Fatal("invalid tool_choice was accepted")
	}
}

func TestNormalizeCodexBodyPreservesFunctionCallOutputContract(t *testing.T) {
	body := map[string]any{
		"model": "gpt-5.6-sol",
		"input": []any{map[string]any{
			"id":      "transient-item-id",
			"type":    "function_call_output",
			"call_id": "call_goal_1",
			"output":  "tests passed",
		}},
	}
	if err := normalizeCodexBody(body, codexIdentity{}, true); err != nil {
		t.Fatal(err)
	}
	input := body["input"].([]any)
	item := input[len(input)-1].(map[string]any)
	if item["call_id"] != "call_goal_1" || item["output"] != "tests passed" {
		t.Fatalf("function_call_output changed: %#v", item)
	}
	if _, exists := item["id"]; exists {
		t.Fatalf("transient id was retained: %#v", item)
	}
}

func TestNormalizeCodexHTTPRequestPreservesDistinctClientRequestID(t *testing.T) {
	req, err := http.NewRequest(http.MethodPost, "https://chatgpt.com/backend-api/codex/responses", strings.NewReader(`{"model":"gpt-5.6-sol","input":"hello"}`))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("session_id", "session-1")
	req.Header.Set("x-client-request-id", "request-123")
	if err := normalizeCodexHTTPRequest(req, "0.144.0"); err != nil {
		t.Fatal(err)
	}
	if got := req.Header.Get("session-id"); got != "session-1" {
		t.Fatalf("session-id = %q", got)
	}
	if got := req.Header.Get("x-client-request-id"); got != "request-123" {
		t.Fatalf("x-client-request-id = %q", got)
	}
}
