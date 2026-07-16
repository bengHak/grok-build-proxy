package modelmap

import (
	"reflect"
	"strings"
	"testing"
)

func TestParseAndResolve(t *testing.T) {
	mapping, err := Parse("grok-build=gpt-5.6-terra, grok-4.5 = gpt-5.6-sol")
	if err != nil {
		t.Fatal(err)
	}
	got := mapping.Resolve("grok-4.5")
	if got.Model != "gpt-5.6-sol" || !got.Mapped || got.Fast {
		t.Fatalf("resolution = %#v", got)
	}
	if !reflect.DeepEqual(got.Chain, []string{"grok-4.5", "gpt-5.6-sol"}) {
		t.Fatalf("chain = %#v", got.Chain)
	}
}

func TestResolvePropagatesFastSuffix(t *testing.T) {
	mapping, err := Parse("grok-4.5=gpt-5.6-sol")
	if err != nil {
		t.Fatal(err)
	}
	got := mapping.Resolve("grok-4.5-fast")
	if got.Model != "gpt-5.6-sol" || !got.Fast || !got.Mapped {
		t.Fatalf("resolution = %#v", got)
	}
}

func TestResolveTargetFastSuffix(t *testing.T) {
	mapping, err := Parse("grok-build=gpt-5.6-terra-fast")
	if err != nil {
		t.Fatal(err)
	}
	got := mapping.Resolve("grok-build")
	if got.Model != "gpt-5.6-terra" || !got.Fast {
		t.Fatalf("resolution = %#v", got)
	}
}

func TestResolveExactFastSourceBeforeBaseSource(t *testing.T) {
	mapping, err := Parse("grok-4.5=gpt-5.6-sol,grok-4.5-fast=gpt-5.6-luna")
	if err != nil {
		t.Fatal(err)
	}
	got := mapping.Resolve("grok-4.5-fast")
	if got.Model != "gpt-5.6-luna" || !got.Fast {
		t.Fatalf("resolution = %#v", got)
	}
}

func TestResolveChainedSubstitution(t *testing.T) {
	mapping, err := Parse("composer=grok-build\ngrok-build=gpt-5.6-terra")
	if err != nil {
		t.Fatal(err)
	}
	got := mapping.Resolve("composer")
	if got.Model != "gpt-5.6-terra" {
		t.Fatalf("resolution = %#v", got)
	}
	want := []string{"composer", "grok-build", "gpt-5.6-terra"}
	if !reflect.DeepEqual(got.Chain, want) {
		t.Fatalf("chain = %#v, want %#v", got.Chain, want)
	}
}

func TestResolveWithoutMappingPassesThrough(t *testing.T) {
	got := (Map{}).Resolve("gpt-5.6-sol-fast")
	if got.Model != "gpt-5.6-sol" || !got.Fast || got.Mapped {
		t.Fatalf("resolution = %#v", got)
	}
}

func TestParseRejectsMalformedAndCyclicMappings(t *testing.T) {
	for _, input := range []string{
		"missing-separator",
		"=gpt-5.6-sol",
		"grok-build=",
		"grok-build=grok-build",
		"a=b,a=c",
		"a=b,b=a",
		"a=b,b=b-fast",
		"grok build=gpt-5.6-terra",
		"grok-build=gpt 5.6 terra",
	} {
		t.Run(strings.ReplaceAll(input, "/", "_"), func(t *testing.T) {
			if _, err := Parse(input); err == nil {
				t.Fatalf("Parse(%q) unexpectedly succeeded", input)
			}
		})
	}
}

func TestStringIsStable(t *testing.T) {
	mapping, err := Parse("z=two,a=one")
	if err != nil {
		t.Fatal(err)
	}
	if got, want := mapping.String(), "a=one,z=two"; got != want {
		t.Fatalf("String() = %q, want %q", got, want)
	}
}
