package catalog

import (
	"sort"
	"strings"
)

// ReasoningEffort describes one reasoning level exposed to Grok Build.
type ReasoningEffort struct {
	Value       string `json:"value"`
	Label       string `json:"label"`
	Description string `json:"description"`
	Default     bool   `json:"default"`
}

// ReasoningCapability describes the reasoning levels supported by a model.
type ReasoningCapability struct {
	DefaultEffort string
	Efforts       []ReasoningEffort
}

// Model describes the small amount of model metadata the proxy needs for
// request shaping and Grok Build's model picker.
type Model struct {
	ID            string               `json:"id"`
	DisplayName   string               `json:"display_name,omitempty"`
	Description   string               `json:"description,omitempty"`
	ContextWindow int                  `json:"context_window,omitempty"`
	ResponsesLite bool                 `json:"-"`
	Reasoning     *ReasoningCapability `json:"-"`
}

func reasoningCapability(defaultEffort string) *ReasoningCapability {
	efforts := []ReasoningEffort{
		{Value: "low", Label: "Low", Description: "Faster responses with lighter reasoning."},
		{Value: "medium", Label: "Medium", Description: "Balanced reasoning for most tasks."},
		{Value: "high", Label: "High", Description: "Deeper reasoning for complex tasks."},
		{Value: "xhigh", Label: "Extra high", Description: "Maximum supported reasoning depth."},
	}
	for i := range efforts {
		efforts[i].Default = efforts[i].Value == defaultEffort
	}
	return &ReasoningCapability{DefaultEffort: defaultEffort, Efforts: efforts}
}

func cloneModel(model Model) Model {
	if model.Reasoning == nil {
		return model
	}
	capability := *model.Reasoning
	capability.Efforts = append([]ReasoningEffort(nil), capability.Efforts...)
	model.Reasoning = &capability
	return model
}

var knownModels = map[string]Model{
	"gpt-5.6-sol": {
		ID:            "gpt-5.6-sol",
		DisplayName:   "GPT-5.6 Sol",
		Description:   "Latest frontier agentic coding model.",
		ContextWindow: 372000,
		ResponsesLite: true,
		Reasoning:     reasoningCapability("low"),
	},
	"gpt-5.6-terra": {
		ID:            "gpt-5.6-terra",
		DisplayName:   "GPT-5.6 Terra",
		Description:   "Balanced agentic coding model for everyday work.",
		ContextWindow: 372000,
		ResponsesLite: true,
		Reasoning:     reasoningCapability("medium"),
	},
	"gpt-5.6-luna": {
		ID:            "gpt-5.6-luna",
		DisplayName:   "GPT-5.6 Luna",
		Description:   "Fast agentic coding model.",
		ContextWindow: 372000,
		ResponsesLite: true,
		Reasoning:     reasoningCapability("medium"),
	},
	"gpt-5.5": {
		ID:            "gpt-5.5",
		DisplayName:   "GPT-5.5",
		Description:   "Frontier model for complex coding and real-world work.",
		ContextWindow: 272000,
		Reasoning:     reasoningCapability("medium"),
	},
	"gpt-5.2": {
		ID:            "gpt-5.2",
		DisplayName:   "GPT-5.2",
		Description:   "Model optimized for professional work and long-running agents.",
		ContextWindow: 272000,
	},
}

var defaultIDs = []string{
	"gpt-5.6-sol",
	"gpt-5.6-terra",
	"gpt-5.6-luna",
	"gpt-5.5",
	"gpt-5.2",
}

type Catalog struct {
	models map[string]Model
	order  []string
}

// New builds a catalog from a comma-separated allow-list. An empty string uses
// the current built-in defaults. Unknown IDs are accepted as full Responses API
// models so users can try newly enabled account-specific models without a new
// proxy release.
func New(csv string) Catalog {
	ids := splitIDs(csv)
	if len(ids) == 0 {
		ids = append([]string(nil), defaultIDs...)
	}
	models := make(map[string]Model, len(ids))
	order := make([]string, 0, len(ids))
	for _, id := range ids {
		base, _ := NormalizeID(id)
		if _, exists := models[base]; exists {
			continue
		}
		model, ok := knownModels[base]
		if !ok {
			model = Model{
				ID:            base,
				DisplayName:   base,
				ContextWindow: 272000,
				ResponsesLite: strings.HasPrefix(base, "gpt-5.6-"),
			}
		}
		models[base] = cloneModel(model)
		order = append(order, base)
	}
	return Catalog{models: models, order: order}
}

func splitIDs(csv string) []string {
	seen := map[string]struct{}{}
	var ids []string
	for _, value := range strings.Split(csv, ",") {
		id := strings.TrimSpace(value)
		if id == "" {
			continue
		}
		if _, ok := seen[id]; ok {
			continue
		}
		seen[id] = struct{}{}
		ids = append(ids, id)
	}
	return ids
}

// NormalizeID resolves the proxy's -fast alias to the upstream model ID.
func NormalizeID(id string) (base string, fast bool) {
	id = strings.TrimSpace(id)
	if strings.HasSuffix(id, "-fast") {
		candidate := strings.TrimSuffix(id, "-fast")
		if candidate != "" {
			return candidate, true
		}
	}
	return id, false
}

func (c Catalog) Lookup(id string) (Model, bool) {
	base, _ := NormalizeID(id)
	model, ok := c.models[base]
	if ok {
		return cloneModel(model), true
	}
	// A mapping target may be a built-in Codex model that the user chose not to
	// advertise via --models. Keep its authoritative request-shape metadata.
	if model, ok := knownModels[base]; ok {
		return cloneModel(model), true
	}
	// Unknown account-specific models are allowed through. Infer the only wire
	// distinction currently needed by the proxy from the model family.
	if base != "" {
		return Model{
			ID:            base,
			DisplayName:   base,
			ContextWindow: 272000,
			ResponsesLite: strings.HasPrefix(base, "gpt-5.6-"),
		}, false
	}
	return Model{}, false
}

func (c Catalog) Models() []Model {
	models := make([]Model, 0, len(c.order))
	for _, id := range c.order {
		models = append(models, cloneModel(c.models[id]))
	}
	return models
}

func (c Catalog) IDs() []string {
	ids := append([]string(nil), c.order...)
	return ids
}

// SortedKnownIDs is primarily useful for diagnostics and tests.
func SortedKnownIDs() []string {
	ids := make([]string, 0, len(knownModels))
	for id := range knownModels {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	return ids
}
