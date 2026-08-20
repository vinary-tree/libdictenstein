package main

import (
	"bytes"
	"encoding/json"
	"testing"
)

func TestCorpusChecksumMatchesRustDriver(t *testing.T) {
	for _, test := range []struct {
		entries int
		full    uint64
		early   uint64
	}{
		{entries: 4_096, full: 8_386_560, early: 2_016},
		{entries: 65_536, full: 2_147_450_880, early: 1_968_480},
	} {
		corpus := makeCorpus(test.entries)
		if got := expectedChecksum(corpus, test.entries); got != test.full {
			t.Fatalf("full checksum for %d entries = %d, want %d", test.entries, got, test.full)
		}
		if got := expectedChecksum(corpus, 64); got != test.early {
			t.Fatalf("early checksum for %d entries = %d, want %d", test.entries, got, test.early)
		}
	}
}

func TestSmallMachineReadableArms(t *testing.T) {
	for _, arm := range []string{"materialized", "stream", "stream-cancel"} {
		var output bytes.Buffer
		err := run([]string{
			"--arm", arm,
			"--entries", "16",
			"--passes", "1",
			"--warmup-passes", "1",
			"--batch-size", "4",
			"--early-cancel", "5",
		}, &output)
		if err != nil {
			t.Fatalf("%s arm: %v", arm, err)
		}
		var result profileResult
		if err := json.Unmarshal(output.Bytes(), &result); err != nil {
			t.Fatalf("%s JSON: %v", arm, err)
		}
		if result.Schema != "libdictenstein.host-collection-traversal.v1" || result.Runtime != "go" || result.Arm != arm {
			t.Fatalf("unexpected %s result: %#v", arm, result)
		}
		if result.Checksum == 0 || result.ElapsedNS < 0 {
			t.Fatalf("invalid %s measurement: %#v", arm, result)
		}
	}
}
