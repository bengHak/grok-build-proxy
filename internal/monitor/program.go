package monitor

import (
	"context"
	"fmt"
	"io"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/proxy"
	"golang.org/x/term"
)

type Dashboard struct {
	state   *State
	updates chan struct{}
}

func NewDashboard() *Dashboard {
	return &Dashboard{state: NewState(), updates: make(chan struct{}, 1)}
}

func (d *Dashboard) Observe(event proxy.RequestEvent) {
	d.state.Apply(event)
	select {
	case d.updates <- struct{}{}:
	default:
	}
}

func (d *Dashboard) Snapshot() Snapshot { return d.state.Snapshot() }

type Terminal interface {
	Size() (width, height int)
	EnterRaw() (restore func(), err error)
}

type fileTerminal struct {
	in  *os.File
	out *os.File
}

func NewTerminal(in, out *os.File) Terminal { return fileTerminal{in: in, out: out} }

func (t fileTerminal) Size() (int, int) {
	width, height, err := term.GetSize(int(t.out.Fd()))
	if err != nil || width <= 0 || height <= 0 {
		return 80, 24
	}
	return width, height
}

func (t fileTerminal) EnterRaw() (func(), error) {
	state, err := term.MakeRaw(int(t.in.Fd()))
	if err != nil {
		return nil, err
	}
	var once sync.Once
	return func() { once.Do(func() { _ = term.Restore(int(t.in.Fd()), state) }) }, nil
}

type fdWriter interface{ Fd() uintptr }
type fdReader interface{ Fd() uintptr }

func IsTerminal(w io.Writer) bool {
	file, ok := w.(fdWriter)
	return ok && term.IsTerminal(int(file.Fd()))
}

func IsInteractive(in io.Reader, out io.Writer) bool {
	input, inputOK := in.(fdReader)
	output, outputOK := out.(fdWriter)
	return inputOK && outputOK && term.IsTerminal(int(input.Fd())) && term.IsTerminal(int(output.Fd()))
}

type Program struct {
	Dashboard *Dashboard
	Input     io.Reader
	Output    io.Writer
	Terminal  Terminal
	Address   string
	Version   string
	Refresh   time.Duration
}

func (p *Program) Run(ctx context.Context) error {
	if p.Dashboard == nil || p.Input == nil || p.Output == nil {
		return fmt.Errorf("monitor requires dashboard, input, and output")
	}
	refresh := p.Refresh
	if refresh <= 0 {
		refresh = 250 * time.Millisecond
	}
	restore := func() {}
	if p.Terminal != nil {
		var err error
		restore, err = p.Terminal.EnterRaw()
		if err != nil {
			return fmt.Errorf("enter terminal raw mode: %w", err)
		}
	}
	var cancelInput context.CancelFunc
	var inputDone <-chan struct{}
	defer func() {
		_, _ = io.WriteString(p.Output, "\x1b[0m\x1b[?25h\x1b[?1049l")
		restore()
		if cancelInput != nil {
			cancelInput()
		}
		if closer, ok := p.Input.(io.Closer); ok && inputDone != nil {
			_ = closer.Close()
			<-inputDone
		}
	}()
	if _, err := io.WriteString(p.Output, "\x1b[?1049h\x1b[?25l"); err != nil {
		return err
	}

	bytes := make(chan byte, 16)
	inputCtx, cancel := context.WithCancel(ctx)
	cancelInput = cancel
	done := make(chan struct{})
	inputDone = done
	go func() {
		defer close(done)
		readInput(inputCtx, p.Input, bytes)
	}()
	ticker := time.NewTicker(refresh)
	defer ticker.Stop()
	view := View{Address: p.Address, Version: p.Version}
	decoder := keyDecoder{}

	render := func() error {
		width, height := 80, 24
		if p.Terminal != nil {
			width, height = p.Terminal.Size()
		}
		screen := strings.ReplaceAll(view.Render(p.Dashboard.Snapshot(), width, height), "\n", "\r\n")
		_, err := fmt.Fprintf(p.Output, "\x1b[2J\x1b[H%s", screen)
		return err
	}
	if err := render(); err != nil {
		return err
	}

	for {
		var escape <-chan time.Time
		if decoder.timer != nil {
			escape = decoder.timer.C
		}
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
			if err := render(); err != nil {
				return err
			}
		case <-p.Dashboard.updates:
			if err := render(); err != nil {
				return err
			}
		case <-escape:
			decoder.expire()
			view.HandleKey("escape", p.Dashboard.Snapshot())
			if err := render(); err != nil {
				return err
			}
		case value, ok := <-bytes:
			if !ok {
				bytes = nil
				continue
			}
			for _, key := range decoder.feed(value) {
				if view.HandleKey(key, p.Dashboard.Snapshot()) {
					return nil
				}
			}
			if err := render(); err != nil {
				return err
			}
		}
	}
}
