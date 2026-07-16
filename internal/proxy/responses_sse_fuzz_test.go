package proxy

import (
	"bytes"
	"io"
	"testing"
)

type fixedChunkReader struct {
	data []byte
	size int
}

func (r *fixedChunkReader) Read(p []byte) (int, error) {
	if len(r.data) == 0 {
		return 0, io.EOF
	}
	size := r.size
	if size <= 0 || size > len(r.data) {
		size = len(r.data)
	}
	if size > len(p) {
		size = len(p)
	}
	n := copy(p, r.data[:size])
	r.data = r.data[n:]
	return n, nil
}

func (r *fixedChunkReader) Close() error { return nil }

func FuzzResponsesLiteSSEChunkBoundaries(f *testing.F) {
	stream := []byte(sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_fuzz","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_fuzz","type":"function_call","status":"in_progress","call_id":"call_fuzz","name":"update_goal","arguments":""}}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":2,"item_id":"fc_fuzz","output_index":0,"arguments":"{\"completed\":true}"}`,
		``,
		`data: [DONE]`,
		``,
	))
	baseline, err := io.ReadAll(newResponsesLiteSSEBody(io.NopCloser(bytes.NewReader(stream))))
	if err != nil {
		f.Fatal(err)
	}
	for _, size := range []int{1, 2, 3, 7, 16, 64, len(stream)} {
		f.Add(size)
	}
	f.Fuzz(func(t *testing.T, size int) {
		if size < 1 {
			size = 1
		}
		if size > 4096 {
			size = 4096
		}
		reader := &fixedChunkReader{data: append([]byte(nil), stream...), size: size}
		got, err := io.ReadAll(newResponsesLiteSSEBody(reader))
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(got, baseline) {
			t.Fatalf("chunk size %d changed output\nwant=%s\ngot=%s", size, baseline, got)
		}
	})
}
