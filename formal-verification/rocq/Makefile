# Makefile for ARTrie Rocq/Coq Formal Verification
# Build with: make
# Clean with: make clean

COQC = coqc
COQDEP = coqdep
COQFLAGS = -Q . ARTrie

# Source directories
DIRS = Spec Model Invariants Operations Proofs

# Source files in dependency order
SPEC_FILES = Spec/MapSpec.v Spec/ARTrieSpec.v
MODEL_FILES = Model/Key.v Model/NodeTypes.v Model/Bucket.v Model/PathCompression.v
INVARIANT_FILES = Invariants/StructuralInvariants.v Invariants/TransitionInvariants.v
OPERATION_FILES = # Operations/Lookup.v Operations/Insert.v Operations/Delete.v
PROOF_FILES = # Proofs/MapRefinement.v Proofs/TransitionCorrectness.v

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
	systemd-run --user --scope -p MemoryMax=126G -p CPUQuota=1800% \
		-p IOWeight=30 -p TasksMax=200 $(MAKE) -j1

# Display build order
show-order:
	@echo "Build order:"
	@echo "  1. Model/Key.v"
	@echo "  2. Model/NodeTypes.v"
	@echo "  3. Model/Bucket.v"
	@echo "  4. Model/PathCompression.v"
	@echo "  5. Spec/MapSpec.v"
	@echo "  6. Spec/ARTrieSpec.v"
	@echo "  7. Invariants/StructuralInvariants.v"
	@echo "  8. Invariants/TransitionInvariants.v"

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
