package proxy

import "testing"

func TestResponsesLiteSSENormalizesCompactContentPartLifecycle(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_content_part","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.content_part.added`,
		`data: {"part":{"type":"output_text"}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"delta":"안녕하세요!"}`,
		``,
		`event: response.output_text.done`,
		`data: {"text":"안녕하세요!"}`,
		``,
		`event: response.content_part.done`,
		`data: {"part":{"type":"output_text","text":""}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_content_part","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	for _, eventType := range []string{
		"response.content_part.added",
		"response.content_part.done",
	} {
		event := findSSEEvent(t, raw, eventType)
		if event["type"] != eventType {
			t.Fatalf("%s type = %#v", eventType, event["type"])
		}
		if _, ok := integerValue(event["sequence_number"]); !ok {
			t.Fatalf("%s missing sequence_number: %#v", eventType, event)
		}
		if event["item_id"] != "msg_content_part_0" {
			t.Fatalf("%s item_id = %#v", eventType, event["item_id"])
		}
		if index, ok := integerValue(event["output_index"]); !ok || index != 0 {
			t.Fatalf("%s output_index = %#v", eventType, event["output_index"])
		}
		if index, ok := integerValue(event["content_index"]); !ok || index != 0 {
			t.Fatalf("%s content_index = %#v", eventType, event["content_index"])
		}

		part := jsonObject(event["part"])
		if part["type"] != "output_text" {
			t.Fatalf("%s part type = %#v", eventType, part["type"])
		}
		if _, ok := part["text"].(string); !ok {
			t.Fatalf("%s part text = %#v", eventType, part["text"])
		}
		if _, ok := part["annotations"].([]any); !ok {
			t.Fatalf("%s annotations = %#v", eventType, part["annotations"])
		}
	}

	done := jsonObject(findSSEEvent(t, raw, "response.content_part.done")["part"])
	if done["text"] != "안녕하세요!" {
		t.Fatalf("done text = %#v", done["text"])
	}
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	if got := responseText(response); got != "안녕하세요!" {
		t.Fatalf("terminal text = %q, stream=%s", got, raw)
	}
}

func TestResponsesLiteSSESynthesizesMissingContentPart(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_missing_part","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"delta":"완료했습니다."}`,
		``,
		`event: response.content_part.done`,
		`data: {}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_missing_part","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	event := findSSEEvent(t, raw, "response.content_part.done")
	part := jsonObject(event["part"])
	if part["type"] != "output_text" || part["text"] != "완료했습니다." {
		t.Fatalf("part = %#v, stream=%s", part, raw)
	}
	if _, ok := part["annotations"].([]any); !ok {
		t.Fatalf("annotations = %#v", part["annotations"])
	}
}

func TestResponsesLiteSSENormalizesCompactReasoningEnvelopes(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_reasoning_aux","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.reasoning_summary_part.added`,
		`data: {"part":{"type":"summary_text"}}`,
		``,
		`event: response.reasoning_summary_text.delta`,
		`data: {"delta":"검토 중"}`,
		``,
		`event: response.reasoning_summary_text.done`,
		`data: {"text":"검토 중"}`,
		``,
		`event: response.reasoning_summary_part.done`,
		`data: {"part":{"type":"summary_text","text":"검토 중"}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"delta":"확인했습니다."}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_reasoning_aux","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	for _, eventType := range []string{
		"response.reasoning_summary_part.added",
		"response.reasoning_summary_text.delta",
		"response.reasoning_summary_text.done",
		"response.reasoning_summary_part.done",
	} {
		event := findSSEEvent(t, raw, eventType)
		if event["item_id"] != "msg_reasoning_aux_0" {
			t.Fatalf("%s item_id = %#v", eventType, event["item_id"])
		}
		if index, ok := integerValue(event["output_index"]); !ok || index != 0 {
			t.Fatalf("%s output_index = %#v", eventType, event["output_index"])
		}
		if index, ok := integerValue(event["summary_index"]); !ok || index != 0 {
			t.Fatalf("%s summary_index = %#v", eventType, event["summary_index"])
		}
		if _, ok := integerValue(event["sequence_number"]); !ok {
			t.Fatalf("%s missing sequence_number: %#v", eventType, event)
		}
	}

	addedPart := jsonObject(findSSEEvent(t, raw, "response.reasoning_summary_part.added")["part"])
	if addedPart["type"] != "summary_text" || addedPart["text"] != "" {
		t.Fatalf("added part = %#v", addedPart)
	}
}

func TestResponsesLiteSSENormalizesCompactBackendLifecycleEnvelope(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_backend_aux","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.web_search_call.in_progress`,
		`data: {}`,
		``,
		`event: response.web_search_call.searching`,
		`data: {}`,
		``,
		`event: response.web_search_call.completed`,
		`data: {}`,
		``,
		`event: response.output_text.delta`,
		`data: {"delta":"검색을 마쳤습니다."}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_backend_aux","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	for _, eventType := range []string{
		"response.web_search_call.in_progress",
		"response.web_search_call.searching",
		"response.web_search_call.completed",
	} {
		event := findSSEEvent(t, raw, eventType)
		if event["item_id"] != "msg_backend_aux_0" {
			t.Fatalf("%s item_id = %#v", eventType, event["item_id"])
		}
		if index, ok := integerValue(event["output_index"]); !ok || index != 0 {
			t.Fatalf("%s output_index = %#v", eventType, event["output_index"])
		}
		if _, ok := integerValue(event["sequence_number"]); !ok {
			t.Fatalf("%s missing sequence_number: %#v", eventType, event)
		}
	}
}
