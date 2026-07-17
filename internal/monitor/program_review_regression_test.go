package monitor

import (
	"bytes"
	"context"
	"io"
	"os"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestReadInputStopsWhenContextIsCanceled(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	done := make(chan struct{})
	go func() {
		readInput(ctx, strings.NewReader("x"), make(chan byte))
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("input reader did not stop after cancellation")
	}
}

func TestReadInputFileStopsWhenContextIsCanceled(t *testing.T) {
	reader, writer, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	defer writer.Close()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	done := make(chan struct{})
	go func() {
		readInput(ctx, reader, make(chan byte))
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("file input reader remained blocked after cancellation")
	}
}

type blockingReadCloser struct {
	closed chan struct{}
	once   sync.Once
}

type restoreOrderTerminal struct {
	input               *blockingReadCloser
	restoredBeforeClose bool
}

func (t *restoreOrderTerminal) Size() (int, int) { return 60, 16 }

func (t *restoreOrderTerminal) EnterRaw() (func(), error) {
	return func() {
		select {
		case <-t.input.closed:
		default:
			t.restoredBeforeClose = true
		}
	}, nil
}

func (r *blockingReadCloser) Read([]byte) (int, error) {
	<-r.closed
	return 0, io.ErrClosedPipe
}

func (r *blockingReadCloser) Close() error {
	r.once.Do(func() { close(r.closed) })
	return nil
}

func TestProgramClosesInputToJoinReaderOnCancellation(t *testing.T) {
	input := &blockingReadCloser{closed: make(chan struct{})}
	t.Cleanup(func() { _ = input.Close() })
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	program := Program{Dashboard: NewDashboard(), Input: input, Output: &bytes.Buffer{}, Refresh: time.Hour}
	if err := program.Run(ctx); err != nil {
		t.Fatal(err)
	}
	select {
	case <-input.closed:
	default:
		t.Fatal("program left the input reader blocked")
	}
}

func TestProgramRestoresTerminalBeforeClosingInput(t *testing.T) {
	input := &blockingReadCloser{closed: make(chan struct{})}
	t.Cleanup(func() { _ = input.Close() })
	terminal := &restoreOrderTerminal{input: input}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	program := Program{Dashboard: NewDashboard(), Input: input, Output: &bytes.Buffer{}, Terminal: terminal, Refresh: time.Hour}
	if err := program.Run(ctx); err != nil {
		t.Fatal(err)
	}
	if !terminal.restoredBeforeClose {
		t.Fatal("terminal restore ran after its input descriptor was closed")
	}
}
