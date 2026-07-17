package main

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/bengHak/grok-build-proxy/internal/catalog"
	"github.com/bengHak/grok-build-proxy/internal/modelmap"
)

func renderGrokConfig(listen string, models catalog.Catalog, mappings modelmap.Map) string {
	var builder strings.Builder
	builder.WriteString("# Add selected blocks to ~/.grok/config.toml\n\n")
	builder.WriteString("# Optional global default used by the Quick Start:\n")
	builder.WriteString("# [models]\n")
	builder.WriteString("# default_reasoning_effort = \"xhigh\"\n\n")
	mappedSources := make(map[string]struct{}, mappings.Len())
	for _, entry := range mappings.Entries() {
		mappedSources[entry.Source] = struct{}{}
		resolved := mappings.Resolve(entry.Source)
		target, _ := models.Lookup(resolved.Model)
		targetName := target.DisplayName
		if resolved.Fast {
			targetName += " (Fast)"
		}
		fmt.Fprintf(&builder, "# Proxy mapping: %s -> %s\n", entry.Source, resolved.EffectiveModelID())
		fmt.Fprintf(&builder, "[model.%s]\n", tomlTableKey(entry.Source))
		fmt.Fprintf(&builder, "model = %q\n", entry.Source)
		fmt.Fprintf(&builder, "name = %q\n", displayModelID(entry.Source)+" via Codex "+targetName)
		fmt.Fprintf(&builder, "description = %q\n", fmt.Sprintf("Routes %s to %s through grok-build-proxy", entry.Source, resolved.EffectiveModelID()))
		fmt.Fprintf(&builder, "base_url = %q\n", "http://"+listen+"/v1")
		builder.WriteString("api_backend = \"responses\"\n")
		builder.WriteString("api_key = \"unused\"\n")
		fmt.Fprintf(&builder, "context_window = %d\n\n", target.ContextWindow)
	}
	for _, model := range models.Models() {
		if _, mapped := mappedSources[model.ID]; mapped {
			continue
		}
		name := strings.NewReplacer(".", "-", "_", "-", "/", "-").Replace(model.ID)
		fmt.Fprintf(&builder, "[model.codex-%s]\n", name)
		fmt.Fprintf(&builder, "model = %q\n", model.ID)
		fmt.Fprintf(&builder, "name = %q\n", "Codex "+model.DisplayName)
		fmt.Fprintf(&builder, "base_url = %q\n", "http://"+listen+"/v1")
		builder.WriteString("api_backend = \"responses\"\n")
		builder.WriteString("api_key = \"unused\"\n")
		fmt.Fprintf(&builder, "context_window = %d\n\n", model.ContextWindow)
	}
	return builder.String()
}

func displayModelID(value string) string {
	parts := strings.FieldsFunc(value, func(r rune) bool {
		return r == '-' || r == '_' || r == '/'
	})
	for i, part := range parts {
		switch strings.ToLower(part) {
		case "grok":
			parts[i] = "Grok"
		case "gpt":
			parts[i] = "GPT"
		case "codex":
			parts[i] = "Codex"
		default:
			if part != "" {
				runes := []rune(part)
				runes[0] = []rune(strings.ToUpper(string(runes[0])))[0]
				parts[i] = string(runes)
			}
		}
	}
	if len(parts) == 0 {
		return value
	}
	return strings.Join(parts, " ")
}

func tomlTableKey(value string) string {
	if value != "" {
		bare := true
		for _, r := range value {
			if !((r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') || r == '_' || r == '-') {
				bare = false
				break
			}
		}
		if bare {
			return value
		}
	}
	return strconv.Quote(value)
}
