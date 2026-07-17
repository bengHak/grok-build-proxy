package monitor

import "time"

func (s *State) session(key, id string) *Session {
	if id == "" {
		id = "default"
	}
	if session := s.sessions[key]; session != nil {
		return session
	}
	s.pruneSessions()
	session := &Session{key: key, ID: id}
	s.sessions[key] = session
	return session
}

func updateSessionDisplay(session *Session, model string, seen time.Time) {
	if seen.After(session.LastSeen) {
		session.LastSeen = seen
		if model != "" {
			session.Model = model
		}
		return
	}
	if seen.Equal(session.LastSeen) && model != "" && (session.Model == "" || model < session.Model) {
		session.Model = model
	}
}

func (s *State) pruneSessions() {
	for len(s.sessions) > historyLimit {
		oldestKey := ""
		var oldest *Session
		for key, session := range s.sessions {
			if session.Active != 0 {
				continue
			}
			if oldest == nil || sessionBefore(session, key, oldest, oldestKey) {
				oldestKey = key
				oldest = session
			}
		}
		if oldest == nil {
			return
		}
		delete(s.sessions, oldestKey)
	}
}

func sessionBefore(left *Session, leftKey string, right *Session, rightKey string) bool {
	return left.LastSeen.Before(right.LastSeen) ||
		left.LastSeen.Equal(right.LastSeen) && (left.ID < right.ID || left.ID == right.ID && leftKey < rightKey)
}
