# Makefile for ARTrie Rocq/Coq Formal Verification
# Build with: make
# Clean with: make clean

COQC = coqc
COQDEP = coqdep
COQFLAGS = -Q . ARTrie

# Source directories
DIRS = Spec Model Invariants Operations Proofs

# Source files in dependency order
SPEC_FILES = Spec/MapSpec.v Spec/DictionaryLawSpec.v Spec/DynamicDawgMutationSpec.v Spec/DynamicDawgU64Spec.v Spec/VariableWidthCodecSpec.v Spec/PersistentARTrieU64Spec.v Spec/DoubleArrayTrieSpec.v Spec/ZipperLanguageSpec.v Spec/ValuedSetCombinatorSpec.v Spec/BloomFilterSpec.v Spec/PersistentMergeSpec.v Spec/PersistentPrefixSpec.v Spec/PersistentReadTraversalSpec.v Spec/PersistentSuffixAutomatonSpec.v Spec/PersistentScdawgSpec.v Spec/PersistentCharNodeLayoutSpec.v Spec/PathMapFactorySpec.v Spec/PathMapSnapshotSpec.v Spec/RelativeEncodingSpec.v Spec/ArenaReservationSpec.v Spec/DedupArenaSpec.v Spec/RootDescriptorReopenSpec.v Spec/PersistentLazyMutationSpec.v Spec/PersistentWalAtomicitySpec.v Spec/LockFreeCounterMergeSpec.v Spec/OverlayReestablishSpec.v Spec/SharedPersistentConcurrencySpec.v Spec/PublicDurabilityPolicySpec.v Spec/PersistentPublicWalLifecycleSpec.v Spec/PersistentEndToEndTraceSpec.v Spec/PersistentVocabWalAtomicitySpec.v Spec/PersistentVocabCheckpointSpec.v Spec/PersistentCheckpointRetentionSpec.v Spec/PersistentDirtyCheckpointSpec.v Spec/PersistentWalSegmentLifecycleSpec.v Spec/PersistentRecoveryPlannerSpec.v Spec/PersistentRecoveryReplayCompletenessSpec.v Spec/PersistentCompactionSpec.v Spec/PersistentRewriteCompactionSpec.v Spec/SubstringSearchSpec.v Spec/ScdawgOccurrenceSpec.v Spec/FuzzyCandidateCoverageSpec.v Spec/SerializationRoundtripSpec.v Spec/OverlayArborescenceSerializationSpec.v Spec/ARTrieSpec.v Spec/ReplicatedMapSpec.v Spec/PersistentCharEvictionRegistrySpec.v Spec/EvictionExactRootPublicationSpec.v Spec/HelpedRootResidencySpec.v Spec/PackedResidencyRefinementSpec.v Spec/HelpedCheckpointStampSpec.v Spec/DetachedCallbackSeparationSpec.v Spec/ResidentBudgetEvictionSpec.v Spec/OverlayFaultProvenanceSpec.v Spec/DictionaryNodeReopenTraversalSpec.v Spec/PersistentCharEpochReclamationSpec.v Spec/CharV3TypeEncodingSpec.v Spec/ApiFeatureVisibilitySpec.v Spec/TlcDiagnosticClassifierSpec.v Spec/OverlayDenseCodecSpec.v Spec/AbiTraversalSnapshotSpec.v Spec/AbiPagingProducerSpec.v Spec/AbiStatusMappingSpec.v Spec/AbiDictionaryAlgebraSpec.v
MODEL_FILES = Model/ListCompat.v Model/Key.v Model/NodeTypes.v Model/Bucket.v Model/HotStuff.v Model/PathCompression.v Model/PrefixChunking.v Model/ArenaManager.v Model/SequentialSiblings.v Model/FileSystem.v
INVARIANT_FILES = Invariants/StructuralInvariants.v Invariants/TransitionInvariants.v Invariants/ArenaInvariants.v Invariants/SequentialSiblingsInvariants.v
OPERATION_FILES = # Operations/Lookup.v Operations/Insert.v Operations/Delete.v
PROOF_FILES = Proofs/MapRefinement.v Proofs/StructuralPreservation.v Proofs/ByzantineRecovery.v Proofs/CertifiedReference.v Proofs/HotStuffSafety.v Proofs/ProofCarryingExtraction.v Proofs/FileSystemSafety.v # Proofs/TransitionCorrectness.v

# All source files
VFILES = $(MODEL_FILES) $(SPEC_FILES) $(INVARIANT_FILES) $(OPERATION_FILES) $(PROOF_FILES) WorkerLifecycle.v

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
	rm -f .lia.cache .nia.cache

# Build with resource limits (for memory-intensive proofs)
# Per CLAUDE.md recommendations
build-safe:
	systemd-run --user --unit=libdictenstein-rocq-build-$$$$ \
		--wait --pipe --quiet --collect --working-directory=$(CURDIR) \
		-p MemoryHigh=1G -p MemoryMax=2G -p MemorySwapMax=0 \
		-p CPUQuota=200% -p IOWeight=30 -p TasksMax=64 $(MAKE) -j1

# Display build order
show-order:
	@echo "Build order:"
	@number=1; for source in $(VFILES); do \
		printf "%3d. %s\n" "$$number" "$$source"; \
		number=$$((number + 1)); \
	done

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
