! Uniform facade conformance suite for the Fortran binding.
!
! Instantiates the family C1-C10 contract for Fortran against a live
! libdictenstein shared library:
!   C1 identity/version      test_identity
!   C2 lifecycle/ownership   test_lifecycle       (idempotent close)
!   C3 error mapping + text  test_error_message   (INVALID_UTF8 + thread-local)
!   C5 CRUD + substring      test_crud, test_substring
!   C6 text domains          test_unicode         (U+00E9 round-trip)
!   C7 batch edges           test_batch_sizes     (0/1/255/256/257)
!   persistence round-trip   test_persistence     (create/checkpoint/reopen)
!
! Run from bindings/fortran with the shared library on the linker and loader
! search paths, e.g.:
!   LIBRARY_PATH=../../target/debug LD_LIBRARY_PATH=../../target/debug \
!     fpm test --link-flag "-L../../target/debug"
program test_conformance
  use, intrinsic :: iso_c_binding
  use vinary_tree_libdictenstein
  implicit none

  call test_identity()
  call test_lifecycle()
  call test_error_message()
  call test_crud()
  call test_unicode()
  call test_batch_sizes()
  call test_substring()
  call test_persistence()
  print *, "fortran conformance: all checks passed"

contains

  subroutine check(condition, label)
    logical, intent(in) :: condition
    character(len=*), intent(in) :: label
    if (.not. condition) then
      print *, "FAIL: ", label
      error stop 1
    end if
  end subroutine

  ! C1: identity constants are surfaced through the facade.
  subroutine test_identity()
    call check(abi_version() == 1, "abi_version == 1")
    call check(api_revision() == 4, "api_revision == 4")
  end subroutine

  ! C2: construction reports its kind; close is idempotent (no double free).
  subroutine test_lifecycle()
    type(dictionary) :: d
    integer(c_int32_t) :: st
    call new_dynamic_dawg(d, status=st)
    call check(st == ldict_ok, "dynamic dawg constructed")
    call check(d%kind() == dynamic_dawg_kind, "kind == dynamic dawg")
    call check(iand(d%capabilities(), can_insert) /= 0, "advertises insert")
    call d%close()
    call d%close()
    call check(.true., "double close is a no-op")
  end subroutine

  ! C3: a failing call yields a non-OK status and a non-empty thread-local
  ! message. A lone 0xFF byte is invalid UTF-8 for a Unicode-scalar backend.
  subroutine test_error_message()
    type(dictionary) :: d
    integer(c_int32_t) :: st
    logical :: ins
    character(kind=c_char, len=:), allocatable :: msg
    call new_dynamic_dawg(d, status=st)
    call d%put_text(achar(255), inserted=ins, status=st)
    call check(st /= ldict_ok, "invalid utf-8 insert rejected")
    msg = last_error_message()
    call check(len(msg) > 0, "last_error_message is non-empty after failure")
    call d%close()
  end subroutine

  ! C5: exact CRUD and the optional-u64 value channel (present and absent).
  subroutine test_crud()
    type(dictionary) :: d
    integer(c_int32_t) :: st
    logical :: ins, rem, fnd, hv
    integer(c_int64_t) :: val
    call new_dynamic_dawg(d, status=st)
    call d%put_text("cat", value=1_c_int64_t, inserted=ins, status=st)
    call check(ins, "insert cat")
    call d%put_text("cot", value=2_c_int64_t, inserted=ins)
    call check(ins, "insert cot")
    call d%put_text("cut", inserted=ins)
    call check(ins, "insert cut valueless")
    call check(int(d%length()) == 3, "length == 3")
    call check(d%contains_text("cat"), "contains cat")
    call check(.not. d%contains_text("dog"), "not contains dog")
    call d%get_text("cot", found=fnd, value=val, has_value=hv, status=st)
    call check(fnd .and. hv .and. val == 2, "get cot == 2")
    call d%get_text("cut", found=fnd, value=val, has_value=hv)
    call check(fnd .and. (.not. hv), "cut found valueless")
    call d%remove_text("cot", removed=rem, status=st)
    call check(rem, "remove cot")
    call check(.not. d%contains_text("cot"), "cot gone")
    call check(int(d%length()) == 2, "length == 2 after remove")
    call d%close()
  end subroutine

  ! C6: a multibyte Unicode scalar (U+00E9, UTF-8 C3 A9) round-trips.
  subroutine test_unicode()
    type(dictionary) :: d
    integer(c_int32_t) :: st
    logical :: ins
    character(kind=c_char, len=2) :: eacute
    eacute = achar(195)//achar(169)
    call new_dynamic_dawg(d, status=st)
    call d%put_text(eacute, inserted=ins, status=st)
    call check(st == ldict_ok, "insert e-acute ok")
    call check(ins, "e-acute inserted")
    call check(d%contains_text(eacute), "contains e-acute")
    call d%close()
  end subroutine

  ! C7: single-crossing batch insert across the paging edge cases.
  subroutine test_batch_sizes()
    integer :: sizes(5), s, i, k
    integer(c_int32_t) :: st
    integer(c_size_t) :: n
    type(dictionary) :: d
    character(len=16), allocatable :: terms(:)
    sizes = [0, 1, 255, 256, 257]
    do s = 1, size(sizes)
      k = sizes(s)
      call new_dynamic_dawg(d, status=st)
      allocate(terms(k))
      do i = 1, k
        write(terms(i), '(A,I0)') "t", i
      end do
      call d%put_all(terms, inserted=n, status=st)
      call check(st == ldict_ok, "batch status ok")
      call check(int(n) == k, "batch inserted == size")
      call check(int(d%length()) == k, "length == batch size")
      deallocate(terms)
      call d%close()
    end do
  end subroutine

  ! C5 substring: SCDAWG advertises SUBSTRING and answers membership/frequency.
  subroutine test_substring()
    type(dictionary) :: d
    integer(c_int32_t) :: st
    logical :: ins
    integer(c_size_t) :: freq
    call new_scdawg(d, status=st)
    call check(st == ldict_ok, "scdawg constructed")
    call check(iand(d%capabilities(), can_substring) /= 0, "scdawg advertises substring")
    call d%put_text("cat", inserted=ins, status=st)
    call d%put_text("cot", inserted=ins)
    call d%put_text("cut", inserted=ins)
    call check(d%contains_substring("at"), "substring at present")
    call check(.not. d%contains_substring("xyz"), "substring xyz absent")
    freq = d%substring_frequency("t")
    call check(int(freq) == 3, "frequency t == 3")
    call d%close()
  end subroutine

  ! Persistence: create/checkpoint/free/reopen preserves committed state.
  subroutine test_persistence()
    type(dictionary) :: d
    integer(c_int32_t) :: st
    logical :: ins, fnd, hv
    integer(c_int64_t) :: val
    integer :: clock
    character(len=80) :: path
    call system_clock(clock)
    write(path, '(A,I0,A)') "/tmp/vt_fortran_conformance_", clock, ".part"
    call create_persistent_artrie(d, trim(path), status=st)
    call check(st == ldict_ok, "persistent create ok")
    call check(d%kind() == persistent_artrie_kind, "kind == persistent artrie")
    call check(iand(d%capabilities(), can_checkpoint) /= 0, "advertises checkpoint")
    call d%put_text("cat", value=1_c_int64_t, inserted=ins, status=st)
    call d%put_text("cot", value=2_c_int64_t, inserted=ins)
    call d%checkpoint(status=st)
    call check(st == ldict_ok, "checkpoint ok")
    call d%close()
    call open_persistent_artrie(d, trim(path), status=st)
    call check(st == ldict_ok, "persistent reopen ok")
    call d%get_text("cot", found=fnd, value=val, has_value=hv, status=st)
    call check(fnd .and. hv .and. val == 2, "reopened cot == 2")
    call d%close()
  end subroutine

end program test_conformance
