package monitor

import "crypto/sha256"

func identityKey(value string) string {
	digest := sha256.Sum256([]byte(value))
	return string(digest[:])
}

func (r Request) identity() string {
	if r.key != "" {
		return r.key
	}
	return r.ID
}

func (s Session) identity() string {
	if s.key != "" {
		return s.key
	}
	return s.ID
}
