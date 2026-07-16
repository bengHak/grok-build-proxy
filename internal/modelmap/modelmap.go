// Package modelmap parses and resolves user-defined model substitutions.
//
// A substitution changes only the model ID sent to the Codex backend. Grok
// Build continues to address the source model ID, which makes it possible to
// override built-in entries such as grok-build or grok-4.5 without changing
// Grok Build's agent and subagent configuration.
package modelmap

import (
	"fmt"
	"sort"
	"strings"
	"unicode"

	"github.com/bengHak/grok-build-proxy/internal/catalog"
)

const fastSuffix = "-fast"

// Entry is one source-to-target model substitution as supplied by the user.
type Entry struct {
	Source string
	Target string
}

// Resolution is the effective upstream model for one requested model ID.
type Resolution struct {
	Requested string
	Model     string
	Fast      bool
	Mapped    bool
	Chain     []string
}

// EffectiveModelID returns the user-facing target ID, including the local
// -fast suffix when priority service is enabled. The actual upstream request
// sends Model without the suffix and sets service_tier separately.
func (r Resolution) EffectiveModelID() string {
	if r.Fast && r.Model != "" {
		return r.Model + fastSuffix
	}
	return r.Model
}

// Map is an immutable set of model substitutions. Its zero value is usable.
type Map struct {
	direct  map[string]string
	entries []Entry
}

// Parse accepts comma-, semicolon-, or newline-separated source=target pairs.
// Whitespace around pairs, sources, and targets is ignored.
func Parse(value string) (Map, error) {
	parts := strings.FieldsFunc(value, func(r rune) bool {
		return r == ',' || r == ';' || r == '\n' || r == '\r'
	})
	if len(parts) == 0 {
		return Map{}, nil
	}

	direct := make(map[string]string, len(parts))
	entries := make([]Entry, 0, len(parts))
	for _, part := range parts {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		source, target, ok := strings.Cut(part, "=")
		if !ok {
			return Map{}, fmt.Errorf("invalid model substitution %q: expected source=target", part)
		}
		source = strings.TrimSpace(source)
		target = strings.TrimSpace(target)
		if source == "" || target == "" {
			return Map{}, fmt.Errorf("invalid model substitution %q: source and target are required", part)
		}
		if err := validateModelID(source); err != nil {
			return Map{}, fmt.Errorf("invalid source model %q: %w", source, err)
		}
		if err := validateModelID(target); err != nil {
			return Map{}, fmt.Errorf("invalid target model %q: %w", target, err)
		}
		if strings.Contains(target, "=") {
			return Map{}, fmt.Errorf("invalid model substitution %q: too many '=' separators", part)
		}
		if source == target {
			return Map{}, fmt.Errorf("invalid model substitution %q: source and target are identical", part)
		}
		if previous, exists := direct[source]; exists {
			return Map{}, fmt.Errorf("duplicate model substitution for %q (%q and %q)", source, previous, target)
		}
		direct[source] = target
		entries = append(entries, Entry{Source: source, Target: target})
	}

	mapping := Map{direct: direct, entries: entries}
	for _, entry := range entries {
		if _, err := mapping.resolve(entry.Source); err != nil {
			return Map{}, err
		}
	}
	return mapping, nil
}

func validateModelID(value string) error {
	for _, r := range value {
		if unicode.IsSpace(r) {
			return fmt.Errorf("model IDs cannot contain whitespace")
		}
		if unicode.IsControl(r) {
			return fmt.Errorf("model IDs cannot contain control characters")
		}
	}
	return nil
}

// Empty reports whether no substitutions are configured.
func (m Map) Empty() bool {
	return len(m.direct) == 0
}

// Len returns the number of explicitly configured substitutions.
func (m Map) Len() int {
	return len(m.direct)
}

// Entries returns substitutions in the order supplied by the user.
func (m Map) Entries() []Entry {
	return append([]Entry(nil), m.entries...)
}

// SortedEntries returns substitutions sorted by source ID for deterministic
// diagnostics and API responses.
func (m Map) SortedEntries() []Entry {
	entries := m.Entries()
	sort.Slice(entries, func(i, j int) bool {
		return entries[i].Source < entries[j].Source
	})
	return entries
}

// Resolve returns the final upstream model. Substitutions may chain. A -fast
// suffix on the request or any target in the chain enables priority service and
// is removed before the final model ID is sent upstream.
func (m Map) Resolve(requested string) Resolution {
	resolution, err := m.resolve(requested)
	if err != nil {
		// Parse validates every reachable chain, and Map's fields are private, so
		// this is defensive only. Falling back to the normalized request is safer
		// than routing to an arbitrary partial target.
		base, fast := catalog.NormalizeID(requested)
		return Resolution{
			Requested: strings.TrimSpace(requested),
			Model:     base,
			Fast:      fast,
			Mapped:    false,
			Chain:     []string{strings.TrimSpace(requested)},
		}
	}
	return resolution
}

func (m Map) resolve(requested string) (Resolution, error) {
	requested = strings.TrimSpace(requested)
	baseRequested, requestedFast := catalog.NormalizeID(requested)
	if requested == "" || baseRequested == "" {
		return Resolution{Requested: requested}, nil
	}

	current := requested
	fast := requestedFast
	chain := []string{requested}
	visited := make(map[string]int, len(m.direct)+1)

	for {
		lookup := current
		next, found := m.direct[lookup]
		if !found {
			base, hasFast := catalog.NormalizeID(current)
			fast = fast || hasFast
			lookup = base
			next, found = m.direct[lookup]
			if !found {
				current = base
				break
			}
		} else {
			_, hasFast := catalog.NormalizeID(current)
			fast = fast || hasFast
		}

		if first, seen := visited[lookup]; seen {
			cycle := append(append([]string(nil), chain[first:]...), lookup)
			return Resolution{}, fmt.Errorf("model substitution cycle: %s", strings.Join(cycle, " -> "))
		}
		visited[lookup] = len(chain) - 1
		current = strings.TrimSpace(next)
		chain = append(chain, current)
	}

	finalModel, targetFast := catalog.NormalizeID(current)
	fast = fast || targetFast
	return Resolution{
		Requested: requested,
		Model:     finalModel,
		Fast:      fast,
		Mapped:    len(chain) > 1 || finalModel != baseRequested,
		Chain:     chain,
	}, nil
}

// String returns a stable comma-separated representation.
func (m Map) String() string {
	entries := m.SortedEntries()
	parts := make([]string, 0, len(entries))
	for _, entry := range entries {
		parts = append(parts, entry.Source+"="+entry.Target)
	}
	return strings.Join(parts, ",")
}
