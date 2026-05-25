# Makefile for ARTrie Rocq/Coq Formal Verification
# Build with: make
# Clean with: make clean

COQC = coqc
COQDEP = coqdep
COQFLAGS = -Q . ARTrie

# Source directories
DIRS = Spec Model Invariants Operations Proofs

# Source files in dependency order
SPEC_FILES = Spec/MapSpec.v Spec/DictionaryLawSpec.v Spec/DynamicDawgMutationSpec.v Spec/DynamicDawgU64Spec.v Spec/DoubleArrayTrieSpec.v Spec/ZipperLanguageSpec.v Spec/ValuedSetCombinatorSpec.v Spec/BloomFilterSpec.v Spec/PersistentMergeSpec.v Spec/PersistentPrefixSpec.v Spec/PathMapFactorySpec.v Spec/RelativeEncodingSpec.v Spec/ArenaReservationSpec.v Spec/DedupArenaSpec.v Spec/RootDescriptorReopenSpec.v Spec/PersistentLazyMutationSpec.v Spec/PersistentWalAtomicitySpec.v Spec/PersistentVocabWalAtomicitySpec.v Spec/PersistentCheckpointRetentionSpec.v Spec/PersistentDirtyCheckpointSpec.v Spec/PersistentWalSegmentLifecycleSpec.v Spec/PersistentRecoveryPlannerSpec.v Spec/PersistentRecoveryReplayCompletenessSpec.v Spec/PersistentCompactionSpec.v Spec/SubstringSearchSpec.v Spec/ScdawgOccurrenceSpec.v Spec/FuzzyCandidateCoverageSpec.v Spec/SerializationRoundtripSpec.v Spec/ARTrieSpec.v Spec/ReplicatedMapSpec.v
MODEL_FILES = Model/Key.v Model/NodeTypes.v Model/Bucket.v Model/HotStuff.v Model/PathCompression.v Model/ArenaManager.v Model/SequentialSiblings.v Model/FileSystem.v
INVARIANT_FILES = Invariants/StructuralInvariants.v Invariants/TransitionInvariants.v Invariants/ArenaInvariants.v Invariants/SequentialSiblingsInvariants.v
OPERATION_FILES = # Operations/Lookup.v Operations/Insert.v Operations/Delete.v
PROOF_FILES = Proofs/MapRefinement.v Proofs/StructuralPreservation.v Proofs/ByzantineRecovery.v Proofs/CertifiedReference.v Proofs/HotStuffSafety.v Proofs/ProofCarryingExtraction.v Proofs/FileSystemSafety.v # Proofs/TransitionCorrectness.v

# All source files
VFILES = $(MODEL_FILES) $(SPEC_FILES) $(INVARIANT_FILES) $(OPERATION_FILES) $(PROOF_FILES)

# Generated files
VOFILES = $(VFILES:.v=.vo)
GLOBFILES = $(VFILES:.v=.glob)
VOKFILES = $(VFILES:.v=.vok)
VOSFILES = $(VFILES:.v=.vos)
DEPFILE = .depend

.PHONY: all clean depend

all: $(VOFILES)

# Compile Coq files
%.vo: %.v
	$(COQC) $(COQFLAGS) $<

# Generate dependencies
depend: $(VFILES)
	$(COQDEP) $(COQFLAGS) $(VFILES) > $(DEPFILE)

# Include dependencies if they exist
-include $(DEPFILE)

# Clean generated files
clean:
	rm -f $(VOFILES) $(GLOBFILES) $(VOKFILES) $(VOSFILES) $(DEPFILE)
	rm -f */*.vo */*.glob */*.vok */*.vos
	rm -f *.vo *.glob *.vok *.vos

# Build with resource limits (for memory-intensive proofs)
# Per CLAUDE.md recommendations
build-safe:
	systemd-run --user --scope -p MemoryMax=32G -p CPUQuota=1800% \
		-p IOWeight=30 -p TasksMax=200 $(MAKE) -j1

# Display build order
show-order:
	@echo "Build order:"
	@echo "  1. Model/Key.v"
	@echo "  2. Model/NodeTypes.v"
	@echo "  3. Model/Bucket.v"
	@echo "  4. Model/HotStuff.v"
	@echo "  5. Model/PathCompression.v"
	@echo "  6. Model/ArenaManager.v"
	@echo "  7. Model/SequentialSiblings.v"
	@echo "  8. Model/FileSystem.v"
	@echo "  9. Spec/MapSpec.v"
	@echo " 10. Spec/DictionaryLawSpec.v"
	@echo " 11. Spec/DynamicDawgMutationSpec.v"
	@echo " 12. Spec/DynamicDawgU64Spec.v"
	@echo " 13. Spec/DoubleArrayTrieSpec.v"
	@echo " 14. Spec/ZipperLanguageSpec.v"
	@echo " 15. Spec/ValuedSetCombinatorSpec.v"
	@echo " 16. Spec/BloomFilterSpec.v"
	@echo " 17. Spec/PersistentMergeSpec.v"
	@echo " 18. Spec/PersistentPrefixSpec.v"
	@echo " 19. Spec/PathMapFactorySpec.v"
	@echo " 20. Spec/RelativeEncodingSpec.v"
	@echo " 21. Spec/ArenaReservationSpec.v"
	@echo " 22. Spec/DedupArenaSpec.v"
	@echo " 23. Spec/RootDescriptorReopenSpec.v"
	@echo " 24. Spec/PersistentLazyMutationSpec.v"
	@echo " 25. Spec/PersistentWalAtomicitySpec.v"
	@echo " 26. Spec/PersistentVocabWalAtomicitySpec.v"
	@echo " 27. Spec/PersistentCheckpointRetentionSpec.v"
	@echo " 28. Spec/PersistentDirtyCheckpointSpec.v"
	@echo " 29. Spec/PersistentWalSegmentLifecycleSpec.v"
	@echo " 30. Spec/PersistentRecoveryPlannerSpec.v"
	@echo " 31. Spec/PersistentRecoveryReplayCompletenessSpec.v"
	@echo " 32. Spec/PersistentCompactionSpec.v"
	@echo " 33. Spec/SubstringSearchSpec.v"
	@echo " 34. Spec/ScdawgOccurrenceSpec.v"
	@echo " 35. Spec/FuzzyCandidateCoverageSpec.v"
	@echo " 36. Spec/SerializationRoundtripSpec.v"
	@echo " 37. Spec/ARTrieSpec.v"
	@echo " 38. Spec/ReplicatedMapSpec.v"
	@echo " 39. Invariants/StructuralInvariants.v"
	@echo " 40. Invariants/TransitionInvariants.v"
	@echo " 41. Invariants/ArenaInvariants.v"
	@echo " 42. Invariants/SequentialSiblingsInvariants.v"
	@echo " 43. Proofs/MapRefinement.v"
	@echo " 44. Proofs/StructuralPreservation.v"
	@echo " 45. Proofs/ByzantineRecovery.v"
	@echo " 46. Proofs/CertifiedReference.v"
	@echo " 47. Proofs/HotStuffSafety.v"
	@echo " 48. Proofs/ProofCarryingExtraction.v"
	@echo " 49. Proofs/FileSystemSafety.v"

# Check a single file
check-%: %.v
	$(COQC) $(COQFLAGS) $<

# Help
help:
	@echo "ARTrie Rocq Verification Makefile"
	@echo ""
	@echo "Targets:"
	@echo "  all        - Build all proof files"
	@echo "  clean      - Remove generated files"
	@echo "  depend     - Generate dependency file"
	@echo "  build-safe - Build with resource limits"
	@echo "  show-order - Show build order"
	@echo "  check-X    - Check a single file (e.g., check-Model/Key)"
	@echo ""
	@echo "Files:"
	@echo "  Model/*.v      - Data structure definitions"
	@echo "  Spec/*.v       - Specifications"
	@echo "  Invariants/*.v - Invariant proofs"
	@echo "  Operations/*.v - Operation proofs (TBD)"
	@echo "  Proofs/*.v     - Main theorem proofs (TBD)"
