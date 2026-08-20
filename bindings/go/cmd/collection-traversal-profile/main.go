// Command collection-traversal-profile measures the public Go collection
// facade over a deterministic dictionary revision. Construction and warmup
// are deliberately outside the reported interval.
package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"sort"
	"time"

	ld "github.com/vinary-tree/libdictenstein/bindings/go"
)

const (
	defaultEntries = 65_536
	defaultBatch   = 256
	defaultEarly   = 64
	keyUnits       = 38
)

type profileConfig struct {
	arm          string
	entries      int
	passes       int
	warmupPasses int
	batchSize    int
	earlyCancel  int
}

type corpusEntry struct {
	term  string
	value uint64
}

type profileResult struct {
	Schema                 string `json:"schema"`
	Runtime                string `json:"runtime"`
	Arm                    string `json:"arm"`
	DictionaryEntries      int    `json:"dictionary_entries"`
	ConsumedEntriesPerPass int    `json:"consumed_entries_per_pass"`
	Passes                 int    `json:"passes"`
	WarmupPasses           int    `json:"warmup_passes"`
	BatchSize              *int   `json:"batch_size"`
	EarlyCancel            *int   `json:"early_cancel"`
	ElapsedNS              int64  `json:"elapsed_ns"`
	Checksum               uint64 `json:"checksum"`
}

func parseArguments(arguments []string) (profileConfig, error) {
	config := profileConfig{}
	flags := flag.NewFlagSet("collection-traversal-profile", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	flags.StringVar(&config.arm, "arm", "", "materialized, stream, or stream-cancel")
	flags.IntVar(&config.entries, "entries", defaultEntries, "dictionary entries")
	flags.IntVar(&config.passes, "passes", 1, "timed drain passes")
	flags.IntVar(&config.warmupPasses, "warmup-passes", 1, "untimed drain passes")
	flags.IntVar(&config.batchSize, "batch-size", defaultBatch, "stream batch entries")
	flags.IntVar(&config.earlyCancel, "early-cancel", defaultEarly, "entries consumed before cancellation")
	if err := flags.Parse(arguments); err != nil {
		return profileConfig{}, err
	}
	if flags.NArg() != 0 {
		return profileConfig{}, fmt.Errorf("unexpected positional argument %q", flags.Arg(0))
	}
	if config.arm != "materialized" && config.arm != "stream" && config.arm != "stream-cancel" {
		return profileConfig{}, errors.New("--arm must be materialized, stream, or stream-cancel")
	}
	if config.entries <= 0 || config.passes <= 0 || config.batchSize <= 0 || config.earlyCancel <= 0 {
		return profileConfig{}, errors.New("--entries, --passes, --batch-size, and --early-cancel must be positive")
	}
	if config.warmupPasses < 0 {
		return profileConfig{}, errors.New("--warmup-passes must be nonnegative")
	}
	if config.batchSize > int(^uint(0)>>1)/keyUnits {
		return profileConfig{}, errors.New("--batch-size is too large")
	}
	return config, nil
}

func makeCorpus(size int) []corpusEntry {
	entries := make([]corpusEntry, size)
	for index := range entries {
		entries[index] = corpusEntry{
			term:  fmt.Sprintf("collection/%04x/%08x/shared-suffix", index&0x0fff, index),
			value: uint64(index),
		}
	}
	return entries
}

func expectedChecksum(entries []corpusEntry, limit int) uint64 {
	ordered := append([]corpusEntry(nil), entries...)
	sort.Slice(ordered, func(left, right int) bool { return ordered[left].term < ordered[right].term })
	if limit < len(ordered) {
		ordered = ordered[:limit]
	}
	var checksum uint64
	for _, entry := range ordered {
		checksum += uint64(len(entry.term)) ^ entry.value
	}
	return checksum
}

func buildDictionary(entries []corpusEntry) (*ld.DynamicDawg, error) {
	dictionary, err := ld.NewDynamicDawg(ld.ByteDomain)
	if err != nil {
		return nil, err
	}
	mutations := make([]ld.Entry, len(entries))
	for index := range entries {
		mutations[index] = ld.Entry{Term: entries[index].term, Value: &entries[index].value}
	}
	inserted, err := dictionary.PutAll(mutations)
	if err != nil {
		_ = dictionary.Close()
		return nil, err
	}
	if int(inserted) != len(entries) {
		_ = dictionary.Close()
		return nil, fmt.Errorf("inserted %d of %d generated entries", inserted, len(entries))
	}
	return dictionary, nil
}

func entryChecksum(entry ld.SnapshotEntry) (uint64, error) {
	if entry.Domain != ld.ByteDomain {
		return 0, fmt.Errorf("benchmark expected byte-domain entry, got %d", entry.Domain)
	}
	var value uint64
	if entry.Value != nil {
		value = *entry.Value
	}
	return uint64(len(entry.Bytes)) ^ value, nil
}

func drainMaterialized(dictionary *ld.Dictionary) (uint64, int, error) {
	entries, err := dictionary.Entries()
	if err != nil {
		return 0, 0, err
	}
	var checksum uint64
	for _, entry := range entries {
		item, itemErr := entryChecksum(entry)
		if itemErr != nil {
			return 0, 0, itemErr
		}
		checksum += item
	}
	return checksum, len(entries), nil
}

func drainStream(dictionary *ld.Dictionary, batchSize, limit int, cancel bool) (uint64, int, error) {
	stream, err := dictionary.OpenEntryStream(ld.EntryBatchLimits{
		MaxEntries: uint(batchSize),
		MaxUnits:   uint(batchSize * keyUnits),
		MaxValues:  uint(batchSize),
	})
	if err != nil {
		return 0, 0, err
	}
	defer stream.Close()

	var checksum uint64
	processed := 0
	for processed < limit {
		entry, ok, nextErr := stream.Next()
		if nextErr != nil {
			return 0, processed, nextErr
		}
		if !ok {
			break
		}
		item, itemErr := entryChecksum(entry)
		if itemErr != nil {
			return 0, processed, itemErr
		}
		checksum += item
		processed++
	}
	if cancel {
		if err := stream.Cancel(); err != nil {
			return 0, processed, err
		}
	} else {
		if processed != limit {
			return 0, processed, fmt.Errorf("stream ended after %d of %d entries", processed, limit)
		}
		if _, ok, nextErr := stream.Next(); nextErr != nil || ok {
			if nextErr != nil {
				return 0, processed, nextErr
			}
			return 0, processed, errors.New("stream cardinality exceeds the generated corpus")
		}
	}
	if err := stream.Close(); err != nil {
		return 0, processed, err
	}
	return checksum, processed, nil
}

func drain(dictionary *ld.Dictionary, config profileConfig) (uint64, int, error) {
	switch config.arm {
	case "materialized":
		return drainMaterialized(dictionary)
	case "stream":
		return drainStream(dictionary, config.batchSize, config.entries, false)
	case "stream-cancel":
		return drainStream(dictionary, config.batchSize, min(config.entries, config.earlyCancel), true)
	default:
		panic("validated arm")
	}
}

func run(arguments []string, output io.Writer) error {
	config, err := parseArguments(arguments)
	if err != nil {
		return err
	}
	corpus := makeCorpus(config.entries)
	dictionary, err := buildDictionary(corpus)
	if err != nil {
		return err
	}
	defer dictionary.Close()

	consumed := config.entries
	if config.arm == "stream-cancel" {
		consumed = min(config.entries, config.earlyCancel)
	}
	expected := expectedChecksum(corpus, consumed)
	for pass := 0; pass < config.warmupPasses; pass++ {
		checksum, count, warmupErr := drain(dictionary.Dictionary, config)
		if warmupErr != nil {
			return warmupErr
		}
		if count != consumed || checksum != expected {
			return fmt.Errorf("warmup mismatch: entries=%d checksum=%d; expected entries=%d checksum=%d", count, checksum, consumed, expected)
		}
	}

	started := time.Now()
	var checksum uint64
	for pass := 0; pass < config.passes; pass++ {
		passChecksum, count, drainErr := drain(dictionary.Dictionary, config)
		if drainErr != nil {
			return drainErr
		}
		if count != consumed || passChecksum != expected {
			return fmt.Errorf("timed drain mismatch: entries=%d checksum=%d; expected entries=%d checksum=%d", count, passChecksum, consumed, expected)
		}
		checksum += passChecksum
	}
	elapsed := time.Since(started).Nanoseconds()
	if checksum != expected*uint64(config.passes) {
		return errors.New("aggregate checksum mismatch")
	}

	result := profileResult{
		Schema:                 "libdictenstein.host-collection-traversal.v1",
		Runtime:                "go",
		Arm:                    config.arm,
		DictionaryEntries:      config.entries,
		ConsumedEntriesPerPass: consumed,
		Passes:                 config.passes,
		WarmupPasses:           config.warmupPasses,
		ElapsedNS:              elapsed,
		Checksum:               checksum,
	}
	if config.arm != "materialized" {
		result.BatchSize = &config.batchSize
	}
	if config.arm == "stream-cancel" {
		result.EarlyCancel = &config.earlyCancel
	}
	return json.NewEncoder(output).Encode(result)
}

func main() {
	if err := run(os.Args[1:], os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
}
