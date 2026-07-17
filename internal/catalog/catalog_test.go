package catalog

import (
	"reflect"
	"testing"
)

func TestDefaultCatalog(t *testing.T) {
	catalog := New("")
	if !reflect.DeepEqual(catalog.IDs(), defaultIDs) {
		t.Fatalf("IDs = %#v, want %#v", catalog.IDs(), defaultIDs)
	}
	model, ok := catalog.Lookup("gpt-5.6-terra")
	if !ok || !model.ResponsesLite || model.ContextWindow != 372000 {
		t.Fatalf("model = %#v, ok = %v", model, ok)
	}
}

func TestKnownReasoningCapabilities(t *testing.T) {
	catalog := New("")
	cases := map[string]string{
		"gpt-5.6-sol":   "low",
		"gpt-5.6-terra": "medium",
		"gpt-5.6-luna":  "medium",
		"gpt-5.5":       "medium",
	}
	wantValues := []string{"low", "medium", "high", "xhigh"}
	for id, wantDefault := range cases {
		model, ok := catalog.Lookup(id)
		if !ok || model.Reasoning == nil {
			t.Fatalf("Lookup(%q) = %#v, %v; want reasoning capability", id, model, ok)
		}
		if model.Reasoning.DefaultEffort != wantDefault {
			t.Errorf("Lookup(%q) default = %q, want %q", id, model.Reasoning.DefaultEffort, wantDefault)
		}
		gotValues := make([]string, 0, len(model.Reasoning.Efforts))
		defaults := 0
		for _, effort := range model.Reasoning.Efforts {
			gotValues = append(gotValues, effort.Value)
			if effort.Default {
				defaults++
				if effort.Value != wantDefault {
					t.Errorf("Lookup(%q) marks %q as default, want %q", id, effort.Value, wantDefault)
				}
			}
		}
		if !reflect.DeepEqual(gotValues, wantValues) || defaults != 1 {
			t.Errorf("Lookup(%q) efforts = %#v with %d defaults, want %#v with one default", id, gotValues, defaults, wantValues)
		}
	}

	unsupported, _ := catalog.Lookup("gpt-5.2")
	if unsupported.Reasoning != nil {
		t.Fatalf("gpt-5.2 reasoning = %#v, want nil", unsupported.Reasoning)
	}
}

func TestFastLookupInheritsReasoningWithoutSharingMutableEfforts(t *testing.T) {
	catalog := New("")
	fast, ok := catalog.Lookup("gpt-5.6-sol-fast")
	if !ok || fast.Reasoning == nil || fast.Reasoning.DefaultEffort != "low" {
		t.Fatalf("fast model = %#v, ok = %v", fast, ok)
	}
	fast.Reasoning.Efforts[0].Value = "mutated"

	canonical, _ := catalog.Lookup("gpt-5.6-sol")
	if got := canonical.Reasoning.Efforts[0].Value; got != "low" {
		t.Fatalf("canonical effort mutated through lookup result: %q", got)
	}
}

func TestCustomCatalogAcceptsUnknownModels(t *testing.T) {
	catalog := New("future-model,gpt-5.6-orbit,future-model")
	if got, want := catalog.IDs(), []string{"future-model", "gpt-5.6-orbit"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("IDs = %#v, want %#v", got, want)
	}
	future, ok := catalog.Lookup("future-model")
	if !ok || future.ResponsesLite || future.Reasoning != nil {
		t.Fatalf("future model = %#v, ok = %v", future, ok)
	}
	orbit, ok := catalog.Lookup("gpt-5.6-orbit-fast")
	if !ok || !orbit.ResponsesLite || orbit.Reasoning != nil {
		t.Fatalf("orbit model = %#v, ok = %v", orbit, ok)
	}
}

func TestNormalizeID(t *testing.T) {
	cases := []struct {
		input string
		base  string
		fast  bool
	}{
		{input: "gpt-5.6-sol", base: "gpt-5.6-sol"},
		{input: "gpt-5.6-sol-fast", base: "gpt-5.6-sol", fast: true},
		{input: " fast ", base: "fast"},
	}
	for _, tc := range cases {
		base, fast := NormalizeID(tc.input)
		if base != tc.base || fast != tc.fast {
			t.Errorf("NormalizeID(%q) = (%q, %v), want (%q, %v)", tc.input, base, fast, tc.base, tc.fast)
		}
	}
}

func TestLookupRetainsKnownMetadataOutsideAdvertisedCatalog(t *testing.T) {
	catalog := New("gpt-5.2")
	model, known := catalog.Lookup("gpt-5.6-sol")
	if !known {
		t.Fatal("gpt-5.6-sol should remain known even when not advertised")
	}
	if !model.ResponsesLite || model.ContextWindow != 372000 {
		t.Fatalf("model = %#v", model)
	}
}
