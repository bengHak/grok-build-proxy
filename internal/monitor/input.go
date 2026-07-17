package monitor

import (
	"context"
	"errors"
	"io"
	"time"

	"golang.org/x/sys/unix"
)

func readInput(ctx context.Context, reader io.Reader, output chan<- byte) {
	defer close(output)
	if file, ok := reader.(fdReader); ok {
		readReadyInput(ctx, reader, int(file.Fd()), output)
		return
	}
	readBlockingInput(ctx, reader, output)
}

func readReadyInput(ctx context.Context, reader io.Reader, fd int, output chan<- byte) {
	var buffer [1]byte
	for {
		ready, err := waitReadable(ctx, fd)
		if err != nil || !ready {
			return
		}
		n, err := reader.Read(buffer[:])
		if n > 0 && !sendInput(ctx, output, buffer[0]) {
			return
		}
		if err != nil {
			return
		}
	}
}

func waitReadable(ctx context.Context, fd int) (bool, error) {
	for {
		if err := ctx.Err(); err != nil {
			return false, err
		}
		var readSet unix.FdSet
		readSet.Set(fd)
		timeout := unix.NsecToTimeval((100 * time.Millisecond).Nanoseconds())
		count, err := unix.Select(fd+1, &readSet, nil, nil, &timeout)
		if errors.Is(err, unix.EINTR) {
			continue
		}
		if err != nil {
			return false, err
		}
		if count > 0 && readSet.IsSet(fd) {
			return true, nil
		}
	}
}

func readBlockingInput(ctx context.Context, reader io.Reader, output chan<- byte) {
	var buffer [1]byte
	for {
		n, err := reader.Read(buffer[:])
		if n > 0 && !sendInput(ctx, output, buffer[0]) {
			return
		}
		if err != nil {
			return
		}
	}
}

func sendInput(ctx context.Context, output chan<- byte, value byte) bool {
	select {
	case output <- value:
		return true
	case <-ctx.Done():
		return false
	}
}

type keyDecoder struct {
	state int
	timer *time.Timer
}

func (d *keyDecoder) feed(value byte) []string {
	if d.timer != nil {
		if !d.timer.Stop() {
			select {
			case <-d.timer.C:
			default:
			}
		}
		d.timer = nil
	}
	switch d.state {
	case 1:
		if value == '[' {
			d.state = 2
			d.timer = time.NewTimer(50 * time.Millisecond)
			return nil
		}
		d.state = 0
		return append([]string{"escape"}, keyName(value)...)
	case 2:
		d.state = 0
		if value == 'A' {
			return []string{"up"}
		}
		if value == 'B' {
			return []string{"down"}
		}
		return nil
	}
	if value == 0x1b {
		d.state = 1
		d.timer = time.NewTimer(50 * time.Millisecond)
		return nil
	}
	return keyName(value)
}

func (d *keyDecoder) expire() {
	d.state = 0
	d.timer = nil
}

func keyName(value byte) []string {
	switch value {
	case 3:
		return []string{"ctrl+c"}
	case '\r', '\n':
		return []string{"enter"}
	case 8, 127:
		return []string{"backspace"}
	case '?', 'q', 'j', 'k':
		return []string{string(value)}
	default:
		return nil
	}
}
