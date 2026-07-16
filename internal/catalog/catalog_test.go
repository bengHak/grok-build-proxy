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

func TestCustomCatalogAcceptsUnknownModels(t *testing.T) {
	catalog := New("future-model,gpt-5.6-orbit,future-model")
	if got, want := catalog.IDs(), []string{"future-model", "gpt-5.6-orbit"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("IDs = %#v, want %#v", got, want)
	}
	future, ok := catalog.Lookup("future-model")
	if !ok || future.ResponsesLite {
		t.Fatalf("future model = %#v, ok = %v", future, ok)
	}
	orbit, ok := catalog.Lookup("gpt-5.6-orbit-fast")
	if !ok || !orbit.ResponsesLite {
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
