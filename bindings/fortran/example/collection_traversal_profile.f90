! Public-package collection traversal benchmark. Construction and warmup are
! outside the timed interval; stdout is one host-collection-traversal.v1 JSON object.
program collection_traversal_profile
  use, intrinsic :: iso_c_binding
  use, intrinsic :: iso_fortran_env, only: real64
  use vinary_tree_libdictenstein
  implicit none

  character(len=24) :: arm = ""
  integer :: entry_count = 65536, passes = 1, warmup_passes = 1
  integer :: batch_size = 256, early_cancel = 64
  integer :: consumed, pass
  integer(c_int64_t) :: expected, checksum, pass_checksum
  integer(c_int64_t) :: started, finished, rate, elapsed_ns
  integer(c_size_t) :: pass_count, inserted
  integer(c_int32_t) :: status
  type(dictionary) :: source
  character(len=38), allocatable :: terms(:)
  integer(c_int64_t), allocatable :: values(:)
  character(len=1024) :: output
  character(len=32) :: batch_json, early_json
  integer(c_int64_t) :: reduce_checksum
  integer(c_size_t) :: reduce_count

  call parse_arguments()
  consumed = entry_count
  if (trim(arm) == "stream-cancel") consumed = min(entry_count, early_cancel)
  expected = expected_checksum(entry_count, consumed)
  allocate(terms(entry_count), values(entry_count))
  do pass = 1, entry_count
    write(terms(pass), '("collection/",Z4.4,"/",Z8.8,"/shared-suffix")') &
      iand(pass - 1, 4095), pass - 1
    values(pass) = int(pass - 1, c_int64_t)
  end do
  call new_dynamic_dawg(source, domain=vt_unit_byte, status=status)
  call require(status == ldict_ok, "could not construct byte dictionary")
  call source%put_all(terms, values=values, inserted=inserted, status=status)
  call require(status == ldict_ok .and. int(inserted) == entry_count, &
    "generated corpus insertion was incomplete")

  do pass = 1, warmup_passes
    call drain(pass_checksum, pass_count)
    call require(pass_checksum == expected .and. int(pass_count) == consumed, &
      "warmup checksum/cardinality mismatch")
  end do
  call system_clock(started, rate)
  checksum = 0
  do pass = 1, passes
    call drain(pass_checksum, pass_count)
    call require(pass_checksum == expected .and. int(pass_count) == consumed, &
      "timed checksum/cardinality mismatch")
    checksum = checksum + pass_checksum
  end do
  call system_clock(finished)
  elapsed_ns = max(1_c_int64_t, int(real(finished - started, real64) * &
    1000000000.0_real64 / real(rate, real64), c_int64_t))

  if (trim(arm) == "materialized") then
    batch_json = "null"
  else
    batch_json = decimal(int(batch_size, c_int64_t))
  end if
  if (trim(arm) == "stream-cancel") then
    early_json = decimal(int(early_cancel, c_int64_t))
  else
    early_json = "null"
  end if
  output = '{"schema":"libdictenstein.host-collection-traversal.v1",'// &
    '"runtime":"fortran","arm":"'//trim(arm)//'","dictionary_entries":'// &
    trim(decimal(int(entry_count, c_int64_t)))//',"consumed_entries_per_pass":'// &
    trim(decimal(int(consumed, c_int64_t)))//',"passes":'// &
    trim(decimal(int(passes, c_int64_t)))//',"warmup_passes":'// &
    trim(decimal(int(warmup_passes, c_int64_t)))//',"batch_size":'// &
    trim(batch_json)//',"early_cancel":'//trim(early_json)//',"elapsed_ns":'// &
    trim(decimal(elapsed_ns))//',"checksum":'//trim(decimal(checksum))//'}'
  write(*, '(A)') trim(output)
  call source%close()

contains

  subroutine parse_arguments()
    integer :: position, count
    character(len=64) :: name, value
    count = command_argument_count()
    position = 1
    do while (position <= count)
      call get_command_argument(position, name)
      call require(position < count, "incomplete benchmark argument")
      call get_command_argument(position + 1, value)
      select case (trim(name))
      case ("--arm"); arm = trim(value)
      case ("--entries"); read(value, *) entry_count
      case ("--passes"); read(value, *) passes
      case ("--warmup-passes"); read(value, *) warmup_passes
      case ("--batch-size"); read(value, *) batch_size
      case ("--early-cancel"); read(value, *) early_cancel
      case default; call require(.false., "unknown benchmark argument")
      end select
      position = position + 2
    end do
    call require(trim(arm) == "materialized" .or. trim(arm) == "stream" .or. &
      trim(arm) == "stream-cancel" .or. trim(arm) == "reduce", &
      "--arm must be materialized, stream, stream-cancel, or reduce")
    call require(entry_count > 0 .and. passes > 0 .and. batch_size > 0 .and. &
      early_cancel > 0 .and. warmup_passes >= 0, "invalid benchmark argument")
    call require(batch_size <= huge(0) / 38, "--batch-size is too large")
  end subroutine

  function expected_checksum(total, limit) result(value)
    integer, intent(in) :: total, limit
    integer(c_int64_t) :: value
    integer :: residue, index, seen
    value = 0
    seen = 0
    do residue = 0, min(4095, total - 1)
      index = residue
      do while (index < total .and. seen < limit)
        value = value + ieor(38_c_int64_t, int(index, c_int64_t))
        seen = seen + 1
        index = index + 4096
      end do
      if (seen == limit) exit
    end do
  end function

  function entry_checksum(entry) result(value)
    type(dictionary_entry), intent(in) :: entry
    integer(c_int64_t) :: value
    call require(entry%unit_domain == vt_unit_byte .and. allocated(entry%bytes), &
      "benchmark expected byte-domain entries")
    value = ieor(int(size(entry%bytes), c_int64_t), merge(entry%value, 0_c_int64_t, entry%has_value))
  end function

  subroutine drain(out_checksum, out_count)
    integer(c_int64_t), intent(out) :: out_checksum
    integer(c_size_t), intent(out) :: out_count
    select case (trim(arm))
    case ("materialized"); call drain_materialized(out_checksum, out_count)
    case ("reduce")
      reduce_checksum = 0
      reduce_count = 0
      call source%fold_entries(reduce_entry, status=status, max_entries=int(batch_size, c_size_t))
      call require(status == ldict_ok, "entries reduce failed")
      out_checksum = reduce_checksum
      out_count = reduce_count
    case default; call drain_stream(out_checksum, out_count)
    end select
  end subroutine

  subroutine drain_materialized(out_checksum, out_count)
    integer(c_int64_t), intent(out) :: out_checksum
    integer(c_size_t), intent(out) :: out_count
    type(dictionary_entry_cursor) :: cursor
    type(dictionary_entry_batch) :: batch
    type(dictionary_entry), allocatable :: snapshot(:)
    integer :: index, offset
    call source%open_entries(cursor, status=status)
    call require(status == ldict_ok, "entries materializer open failed")
    allocate(snapshot(entry_count))
    offset = 0
    do
      call cursor%next_batch(batch, status=status)
      call require(status == ldict_ok, "entries materializer next failed")
      if (batch%count == 0) exit
      do index = 1, int(batch%count)
        offset = offset + 1
        snapshot(offset) = batch%entries(index)
      end do
    end do
    call cursor%close(status)
    call require(status == ldict_ok .and. offset == entry_count, "entries materializer close/count failed")
    out_checksum = 0
    do index = 1, offset
      out_checksum = out_checksum + entry_checksum(snapshot(index))
    end do
    out_count = int(offset, c_size_t)
  end subroutine

  subroutine drain_stream(out_checksum, out_count)
    integer(c_int64_t), intent(out) :: out_checksum
    integer(c_size_t), intent(out) :: out_count
    type(dictionary_entry_cursor) :: cursor
    type(dictionary_entry_batch) :: batch
    integer :: index
    call source%open_entries(cursor, status=status, max_entries=int(batch_size, c_size_t), &
      max_units=int(batch_size * 38, c_size_t), max_values=int(batch_size, c_size_t))
    call require(status == ldict_ok, "entries stream open failed")
    out_checksum = 0
    out_count = 0
    do while (int(out_count) < consumed)
      call cursor%next_batch(batch, max_entries=int(batch_size, c_size_t), status=status)
      call require(status == ldict_ok, "entries stream next failed")
      if (batch%count == 0) exit
      do index = 1, int(batch%count)
        if (int(out_count) == consumed) exit
        out_checksum = out_checksum + entry_checksum(batch%entries(index))
        out_count = out_count + 1
      end do
    end do
    call cursor%close(status)
    call require(status == ldict_ok, "entries stream close failed")
  end subroutine

  subroutine reduce_entry(batch, stop)
    type(dictionary_entry_batch), intent(in) :: batch
    logical, intent(out) :: stop
    integer :: index
    do index = 1, int(batch%count)
      reduce_checksum = reduce_checksum + entry_checksum(batch%entries(index))
      reduce_count = reduce_count + 1
    end do
    stop = .false.
  end subroutine

  subroutine require(condition, message)
    logical, intent(in) :: condition
    character(len=*), intent(in) :: message
    if (.not. condition) then
      write(*, '(A)') trim(message)
      error stop 2
    end if
  end subroutine

  function decimal(value) result(text)
    integer(c_int64_t), intent(in) :: value
    character(len=32) :: text
    write(text, '(I0)') value
    text = adjustl(text)
  end function
end program collection_traversal_profile
