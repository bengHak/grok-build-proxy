// Package monitor implements the interactive serve dashboard.
package monitor

import (
	"sort"
	"sync"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/proxy"
)

const (
	historyLimit = 50
	dedupLimit   = 200
)

type Request struct {
	ID             string
	SessionID      string
	RequestedModel string
	Model          string
	Status         string
	StatusCode     int
	StartedAt      time.Time
	EndedAt        time.Time
	OutputTokens   int64
	Error          string
}

func (r Request) Duration(now time.Time) time.Duration {
	end := r.EndedAt
	if end.IsZero() {
		end = now
	}
	if r.StartedAt.IsZero() || end.Before(r.StartedAt) {
		return 0
	}
	return end.Sub(r.StartedAt)
}

func (r Request) TokensPerSecond() float64 {
	duration := r.Duration(r.EndedAt).Seconds()
	if r.OutputTokens <= 0 || duration <= 0 {
		return 0
	}
	return float64(r.OutputTokens) / duration
}

type Session struct {
	ID              string
	Model           string
	Requests        int
	Active          int
	OutputTokens    int64
	SampledDuration time.Duration
	LastSeen        time.Time
}

func (s Session) TokensPerSecond() float64 {
	if s.OutputTokens <= 0 || s.SampledDuration <= 0 {
		return 0
	}
	return float64(s.OutputTokens) / s.SampledDuration.Seconds()
}

type Snapshot struct {
	Sessions []Session
	Active   []Request
	Recent   []Request
	Errors   []Request
}

type State struct {
	mu            sync.RWMutex
	active        map[string]Request
	finished      map[string]struct{}
	finishedOrder []string
	sessions      map[string]*Session
	recent        []Request
	errors        []Request
}

func NewState() *State {
	return &State{
		active:   make(map[string]Request),
		finished: make(map[string]struct{}),
		sessions: make(map[string]*Session),
	}
}

// Observe makes State satisfy proxy.Observer.
func (s *State) Observe(event proxy.RequestEvent) { s.Apply(event) }

func (s *State) Apply(event proxy.RequestEvent) {
	s.mu.Lock()
	defer s.mu.Unlock()

	switch event.Type {
	case proxy.RequestStarted:
		if _, exists := s.active[event.RequestID]; exists {
			return
		}
		if _, done := s.finished[event.RequestID]; done {
			return
		}
		request := requestFromEvent(event)
		request.Status = "active"
		s.active[event.RequestID] = request
		session := s.session(event.SessionID)
		session.Model = event.Model
		session.Requests++
		session.Active++
		session.LastSeen = event.StartedAt
	case proxy.RequestCompleted, proxy.RequestFailed:
		if _, done := s.finished[event.RequestID]; done {
			return
		}
		request, exists := s.active[event.RequestID]
		if !exists {
			request = requestFromEvent(event)
			session := s.session(event.SessionID)
			session.Requests++
		}
		delete(s.active, event.RequestID)
		s.markFinished(event.RequestID)
		mergeEvent(&request, event)
		if event.Type == proxy.RequestCompleted {
			request.Status = "complete"
		} else {
			request.Status = "failed"
		}
		s.prepend(&s.recent, request)
		session := s.session(request.SessionID)
		if session.Active > 0 {
			session.Active--
		}
		session.Model = request.Model
		session.LastSeen = request.EndedAt
		if request.OutputTokens > 0 && request.Duration(request.EndedAt) > 0 {
			session.OutputTokens += request.OutputTokens
			session.SampledDuration += request.Duration(request.EndedAt)
		}
		if event.Type == proxy.RequestFailed {
			s.prepend(&s.errors, request)
		}
		s.pruneSessions()
	}
}

func (s *State) Snapshot() Snapshot {
	s.mu.RLock()
	defer s.mu.RUnlock()
	result := Snapshot{
		Sessions: make([]Session, 0, len(s.sessions)),
		Active:   make([]Request, 0, len(s.active)),
		Recent:   append([]Request(nil), s.recent...),
		Errors:   append([]Request(nil), s.errors...),
	}
	for _, session := range s.sessions {
		result.Sessions = append(result.Sessions, *session)
	}
	for _, request := range s.active {
		result.Active = append(result.Active, request)
	}
	sort.Slice(result.Sessions, func(i, j int) bool { return result.Sessions[i].LastSeen.After(result.Sessions[j].LastSeen) })
	sort.Slice(result.Active, func(i, j int) bool { return result.Active[i].StartedAt.Before(result.Active[j].StartedAt) })
	return result
}

func (s *State) session(id string) *Session {
	if id == "" {
		id = "default"
	}
	if session := s.sessions[id]; session != nil {
		return session
	}
	s.pruneSessions()
	session := &Session{ID: id}
	s.sessions[id] = session
	return session
}

func (s *State) markFinished(id string) {
	s.finished[id] = struct{}{}
	s.finishedOrder = append(s.finishedOrder, id)
	if len(s.finishedOrder) > dedupLimit {
		oldest := s.finishedOrder[0]
		s.finishedOrder = s.finishedOrder[1:]
		delete(s.finished, oldest)
	}
}

func (s *State) pruneSessions() {
	for len(s.sessions) > historyLimit {
		var oldestID string
		var oldest time.Time
		for id, session := range s.sessions {
			if session.Active != 0 {
				continue
			}
			if oldestID == "" || session.LastSeen.Before(oldest) {
				oldestID = id
				oldest = session.LastSeen
			}
		}
		if oldestID == "" {
			return
		}
		delete(s.sessions, oldestID)
	}
}

func (s *State) prepend(items *[]Request, request Request) {
	*items = append([]Request{request}, *items...)
	if len(*items) > historyLimit {
		*items = (*items)[:historyLimit]
	}
}

func requestFromEvent(event proxy.RequestEvent) Request {
	return Request{
		ID:             event.RequestID,
		SessionID:      event.SessionID,
		RequestedModel: event.RequestedModel,
		Model:          event.Model,
		StatusCode:     event.StatusCode,
		StartedAt:      event.StartedAt,
		EndedAt:        event.EndedAt,
		OutputTokens:   event.OutputTokens,
		Error:          event.Error,
	}
}

func mergeEvent(request *Request, event proxy.RequestEvent) {
	if event.SessionID != "" {
		request.SessionID = event.SessionID
	}
	if event.RequestedModel != "" {
		request.RequestedModel = event.RequestedModel
	}
	if event.Model != "" {
		request.Model = event.Model
	}
	if !event.StartedAt.IsZero() {
		request.StartedAt = event.StartedAt
	}
	request.EndedAt = event.EndedAt
	request.StatusCode = event.StatusCode
	request.OutputTokens = event.OutputTokens
	request.Error = event.Error
}
